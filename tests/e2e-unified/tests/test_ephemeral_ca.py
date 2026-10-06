"""Unit tests for the interop-harness ephemeral CA (tier_1, pure in-process).

The CA + leaf minting is the trust plumbing both sides of the fediverse boundary
depend on (`helpers/ephemeral_ca.py`); a broken chain or a missing SAN would
surface only as an opaque TLS handshake failure deep in the F1 flow, so pin the
mechanism here.
"""

import ipaddress
import sys
from pathlib import Path

import pytest
from cryptography import x509
from cryptography.hazmat.primitives.asymmetric import padding
from cryptography.x509.oid import ExtensionOID

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from helpers.ephemeral_ca import EphemeralCA  # noqa: E402

pytestmark = pytest.mark.tier_1


def test_ca_cert_is_a_ca():
    ca = EphemeralCA()
    cert = x509.load_pem_x509_certificate(ca.cert_pem)
    bc = cert.extensions.get_extension_for_oid(ExtensionOID.BASIC_CONSTRAINTS).value
    assert bc.ca is True


def test_leaf_chains_to_ca():
    """The CA's public key must verify the leaf's signature — else nothing that
    trusts the CA will trust the leaf."""
    ca = EphemeralCA()
    ca_cert = x509.load_pem_x509_certificate(ca.cert_pem)
    leaf = x509.load_pem_x509_certificate(ca.issue("mastodon.test").cert_pem)

    assert leaf.issuer == ca_cert.subject
    # Raises InvalidSignature if the leaf was not signed by this CA.
    ca_cert.public_key().verify(
        leaf.signature,
        leaf.tbs_certificate_bytes,
        padding.PKCS1v15(),
        leaf.signature_hash_algorithm,
    )


def test_leaf_is_not_a_ca():
    ca = EphemeralCA()
    leaf = x509.load_pem_x509_certificate(ca.issue("nest.test").cert_pem)
    bc = leaf.extensions.get_extension_for_oid(ExtensionOID.BASIC_CONSTRAINTS).value
    assert bc.ca is False


def test_hostname_leaf_has_dns_san():
    ca = EphemeralCA()
    leaf = x509.load_pem_x509_certificate(ca.issue("mastodon.test").cert_pem)
    san = leaf.extensions.get_extension_for_oid(ExtensionOID.SUBJECT_ALTERNATIVE_NAME).value
    assert san.get_values_for_type(x509.DNSName) == ["mastodon.test"]


def test_ip_leaf_has_ip_san():
    """An IP `hostname` becomes an IPAddress SAN (a DNSName SAN of an IP is not
    honored by strict verifiers) — the harness may point Mastodon at a bare IP."""
    ca = EphemeralCA()
    leaf = x509.load_pem_x509_certificate(ca.issue("127.0.0.1").cert_pem)
    san = leaf.extensions.get_extension_for_oid(ExtensionOID.SUBJECT_ALTERNATIVE_NAME).value
    assert san.get_values_for_type(x509.IPAddress) == [ipaddress.ip_address("127.0.0.1")]


def test_write_produces_readable_files(tmp_path):
    ca = EphemeralCA()
    ca_path = ca.write_cert(str(tmp_path))
    cert_path, key_path = ca.issue("nest.test").write(str(tmp_path))

    # Every written artifact round-trips through its parser — a truncated or
    # mis-encoded file fails loud here, not at container mount time.
    assert x509.load_pem_x509_certificate(Path(ca_path).read_bytes())
    assert x509.load_pem_x509_certificate(Path(cert_path).read_bytes())
    from cryptography.hazmat.primitives.serialization import load_pem_private_key

    assert load_pem_private_key(Path(key_path).read_bytes(), password=None)


def test_each_run_mints_a_distinct_ca():
    """Per-run isolation: two CA instances must not share a key/serial, or a
    stale trust anchor from a prior run could validate this run's leaf."""
    a = x509.load_pem_x509_certificate(EphemeralCA().cert_pem)
    b = x509.load_pem_x509_certificate(EphemeralCA().cert_pem)
    assert a.serial_number != b.serial_number

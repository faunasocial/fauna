"""tier_4 e2e: an **expired trusted default certificate gives way to the self-signed
floor** on the real Docker deploy image (``tls-certificates.md`` § The two-layer
model: "otherwise it serves the floor").

The nest's own apex and ``mail.<apex>`` ride the *default* cert, never a
per-domain entry, so the resolver's per-SNI expiry fallback alone would keep an
expired CA-issued default on the wire — and, read as ``Expiring`` rather than
on-floor, keep the trusted-cert records published (§ D). The default path's
``floor_if_lapsed_trusted`` (``bins/fauna-nest/src/acme.rs``) closes that; its
Rust unit tests pin the resolver half against an in-process resolver. This is the
packaged image doing it end to end: cert-on-disk → cert-watcher hot reload →
``MultiDomainCertResolver::cert_for_sni`` → the live TLS handshake, the
``fauna.tls.cert_status`` read and the ``fauna.dns.list_records`` matrix.

What it asserts:

  1. **A valid trusted cert is served** once planted in the container's
     ``acme_dir`` (exactly where an ACME issuance lands its chain). This first
     flip proves the watcher reload path is live, so the floor read in step 2
     cannot be a reload that never happened.
  2. **An expired trusted cert is not.** Plant a CA-issued chain whose validity
     window lies wholly in the past: the handshake for the nest's own name now
     presents a self-signed leaf that covers that name and carries the stable
     floor key; ``cert_status`` reads ``OnFloorRenewNeeded``; the floor-MX TLSA
     row pinning that key is offered again; and the admin WS-RPC session — the
     apps' own channel — keeps working across the swap.

Same fixture as the DANE coupling test (claimed nest, ``localhost`` registered
as the primary mail domain, no ACME, no ``test-hooks``) — imported, not copied.
"""

import pytest
from cryptography import x509
from cryptography.x509.oid import ExtensionOID

from helpers.tls_spki import served_leaf, spki_sha256_hex
from helpers.waiting import wait_until

from .helpers import install_acme_default_cert, make_ca_issued_leaf_pem, nest_served_leaf
from .test_dane_tlsa_cert_coupling import (  # noqa: F401 — fixtures
    DOMAIN,
    MX_SNI,
    _floor_mx_tlsa_rows,
    _list_records,
    dane_nest,
    docker_image,
    pytestmark,
)
from .test_acme_http01_pebble_issuance import _cert_status

# The watcher debounces 2 s; leave generous room on a loaded box.
RELOAD_DEADLINE_S = 30


def _is_self_signed(cert: x509.Certificate) -> bool:
    return cert.issuer == cert.subject


def _dns_names(cert: x509.Certificate) -> list[str]:
    san = cert.extensions.get_extension_for_oid(ExtensionOID.SUBJECT_ALTERNATIVE_NAME)
    return [n.lower() for n in san.value.get_values_for_type(x509.DNSName)]


def _wait_served(host, port, sni, predicate, what: str) -> x509.Certificate:
    last = {}

    def _served():
        last["leaf"] = leaf = served_leaf(host, port, sni)
        return leaf if predicate(leaf) else None

    def _diagnose():
        leaf = last.get("leaf")
        if leaf is None:
            return f"no handshake completed for SNI {sni!r}"
        return (
            f"the nest still did not serve {what} for SNI {sni!r}: served "
            f"issuer={leaf.issuer.rfc4514_string()} "
            f"subject={leaf.subject.rfc4514_string()} "
            f"not_after={leaf.not_valid_after_utc.isoformat()} "
            f"spki={spki_sha256_hex(leaf)}"
        )

    return wait_until(_served, RELOAD_DEADLINE_S, interval=1, diagnose=_diagnose)


@pytest.mark.feature("certificates-by-themselves")
def test_expired_trusted_default_cert_serves_the_floor(dane_nest):
    host, port, name = "127.0.0.1", dane_nest["port"], dane_nest["name"]

    # 0 — Boot: on the floor. The floor-MX TLSA pins the stable floor key.
    rows = _floor_mx_tlsa_rows(_list_records(dane_nest))
    assert len(rows) == 1, f"exactly one floor-MX TLSA on the floor, got {rows}"
    floor_hex = rows[0]["expected"].split()[-1].lower()
    assert spki_sha256_hex(nest_served_leaf(name, MX_SNI)) == floor_hex

    # 1 — A valid trusted chain is served: the reload path is live.
    good_chain, good_key = make_ca_issued_leaf_pem([DOMAIN, MX_SNI])
    good_hex = spki_sha256_hex(x509.load_pem_x509_certificate(good_chain))
    install_acme_default_cert(name, good_chain, good_key)
    _wait_served(host, port, DOMAIN, lambda c: spki_sha256_hex(c) == good_hex,
                 "the planted valid trusted cert")
    status = _cert_status(dane_nest, [DOMAIN])[DOMAIN]
    assert status["state"] == "ValidTrusted" and status["is_floor"] is False, status
    assert not _floor_mx_tlsa_rows(_list_records(dane_nest)), (
        "a valid trusted cert must withdraw the floor-MX TLSA"
    )

    # 2 — An expired trusted chain: the floor stands in for it.
    old_chain, old_key = make_ca_issued_leaf_pem([DOMAIN, MX_SNI], expired=True)
    old_leaf = x509.load_pem_x509_certificate(old_chain)
    old_hex = spki_sha256_hex(old_leaf)
    assert not _is_self_signed(old_leaf)
    install_acme_default_cert(name, old_chain, old_key)
    served = _wait_served(
        host, port, DOMAIN,
        lambda c: spki_sha256_hex(c) not in (good_hex, old_hex),
        "a replacement for the expired trusted cert",
    )
    assert _is_self_signed(served), (
        "an expired trusted default must give way to the self-signed floor, got "
        f"issuer={served.issuer.rfc4514_string()}"
    )
    assert DOMAIN in _dns_names(served), (
        f"the floor served for {DOMAIN!r} must cover it, covers {_dns_names(served)}"
    )
    assert spki_sha256_hex(served) == floor_hex, (
        "the stand-in floor must carry the stable floor key (the key the TLSA pins)"
    )
    mx = nest_served_leaf(name, MX_SNI)
    assert _is_self_signed(mx) and spki_sha256_hex(mx) == floor_hex, (
        f"{MX_SNI} must also fall to the floor, served spki={spki_sha256_hex(mx)}"
    )

    # The apps' channel keeps working, and every reader agrees it is the floor.
    status = _cert_status(dane_nest, [DOMAIN])[DOMAIN]
    assert status["state"] == "OnFloorRenewNeeded" and status["is_floor"] is True, status
    rows = _floor_mx_tlsa_rows(_list_records(dane_nest))
    assert [r["expected"].split()[-1].lower() for r in rows] == [floor_hex], (
        f"the floor-MX TLSA pinning the floor key must be offered again, got {rows}"
    )

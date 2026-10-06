"""tier_4 e2e: the self-signed-MX **DANE/TLSA ↔ cert-honesty coupling** proven on
the real Docker deploy image.

This is the real-HTTPS acceptance the tier_3 conformance test
(``bins/fauna-nest/tests/conformance_dns_dane_coupling.rs``) explicitly defers to
the docker slice: that test dispatches the registered ``fauna.dns.list_records``
handler against an in-process ``MultiDomainCertResolver``; here the **packaged
image** does it, with the nest's own boot-time self-signed floor cert serving a
real HTTPS listener and ``state.served_cert_spki`` wired through ``main.rs``. Only
this tier exercises the whole serving stack — self-signed bootstrap →
``MultiDomainCertResolver`` → ``AppState.served_cert_spki`` →
``dns_handlers::append_floor_mx_tlsa`` → ``fauna.dns.list_records`` over admin
WS-RPC — that the tier_3 test stubs by constructing the resolver directly. It is
the image/packaging guarantee: that the SAN-set + cert-watcher + DNS-handler
wiring survives the Dockerfile (which builds ``--features bluesky,nostr,activitypub`` —
**no** ``test-hooks``, so there is no override to script the served cert; the flip
below uses the real cert path).

What it asserts (``tls-certificates.md`` § D — the cert-honesty coupling):

  1. **Floor ⇒ present + pinned to the served leaf.** On the self-signed floor a
     ``register_primary_domain``'d nest publishes exactly one host-level
     ``_25._tcp.mail.<primary> TLSA 3 1 1 <hex>`` on the primary domain, and the
     ``<hex>`` equals ``sha256(SubjectPublicKeyInfo)`` of the cert the image
     **actually serves** for SNI ``mail.<primary>`` (read off the live TLS
     handshake — the wire-truth tier_4 adds over the tier_3 resolver read).
  2. **Floor → trusted ⇒ withdrawn.** Drop a CA-issued leaf (issuer DN ≠ subject
     DN, so ``is_floor == false``) covering the MX into the container's
     ``acme_dir`` — exactly where a real ACME issuance lands the renewed chain.
     The cert-watcher hot-reloads it as the default cert; ``list_records`` then
     **withdraws** the floor-key TLSA (a floor-key pin against a trusted cert
     would DANE-fail), in lockstep with the served-cert reality.

**Why no DNSSEC fake-DNS here.** The floor-MX TLSA row is computed from the
*locally served* cert facts (``served_cert_facts`` / ``served_cert_spki_sha256``),
not from any external DNS lookup — ``list_records`` is a pure read of the matrix.
DNSSEC validation only governs the *outbound* DANE path (we as a sender pinning a
peer MX), which is a separate subsystem. So this acceptance needs neither a
signing resolver nor pebble — just the real image and the real cert path.

**Why no mail-serving precondition.** ``append_floor_mx_tlsa`` is cert-coupled,
not address-gated, and ``assemble_domain_views`` emits a domain's matrix once it
is an active primary mail domain (``removed_at IS NULL`` + ``is_primary``) —
``register_primary_domain`` alone reaches that state, so the slow bridge
enrollment / cert-fan-out dance ``test_mail_deploy_lifecycle.py`` drives is not
needed.
"""

import subprocess
import time

import pytest
from cryptography import x509

from helpers.tls_spki import served_leaf_spki_sha256_hex, spki_sha256_hex

from .helpers import (
    IMAGE_TAG,
    admin_ws,
    claim_admin_api,
    docker_build,
    find_free_ports,
    get_repo_root,
    install_acme_default_cert,
    make_ca_issued_leaf_pem,
    register_primary_domain,
    remove_container,
    start_container_with_ports,
    wait_for_health,
)

try:
    subprocess.run(["docker", "info"], capture_output=True, timeout=10)
    HAS_DOCKER = True
except Exception:
    HAS_DOCKER = False

pytestmark = [
    pytest.mark.skipif(not HAS_DOCKER, reason="Docker not available"),
    pytest.mark.tier_4,
    pytest.mark.self_contained_docker,
]

DOMAIN = "localhost"
CLAIM_CODE = "DANE44"
MX_SNI = f"mail.{DOMAIN}"
TLSA_NAME = f"_25._tcp.mail.{DOMAIN}"


# ── Fixtures ──────────────────────────────────────────────────────────


@pytest.fixture(scope="module")
def docker_image():
    """Build the image once for this module (shared tag, layer-cached)."""
    docker_build(get_repo_root())
    yield IMAGE_TAG


@pytest.fixture()
def dane_nest(docker_image):
    """Fresh claimed nest serving HTTPS on its boot-time self-signed floor, with a
    single primary mail domain registered. No mail listeners are brought up — the
    floor-MX TLSA is cert-coupled, not mail-serving-gated. Cleaned up after."""
    (http_port,) = find_free_ports(1)
    name = f"fauna-dane-{http_port}"
    start_container_with_ports(
        name,
        {3000: http_port},
        env={"FAUNA_CLAIM_CODE": CLAIM_CODE, "FAUNA_PORT": "3000"},
        add_hosts={"host.docker.internal": "host-gateway"},
    )
    try:
        wait_for_health(http_port, name)
        admin = claim_admin_api(http_port, CLAIM_CODE, handle="admin")
        nest = {
            "name": name,
            "port": http_port,
            "url": f"https://127.0.0.1:{http_port}",
            "admin": admin,
        }
        register_primary_domain(nest, DOMAIN)
        yield nest
    finally:
        remove_container(name)


# ── Helpers ───────────────────────────────────────────────────────────


def _list_records(nest) -> list[dict]:
    """`fauna.dns.list_records` over admin WS-RPC → the `domains` list."""
    with admin_ws(nest) as admin:
        return admin.call("fauna.dns.list_records", {})["domains"]


def _floor_mx_tlsa_rows(domains: list[dict]) -> list[dict]:
    """Every `_25._tcp.mail.<primary>` TLSA row on the primary domain's matrix."""
    primary = next(d for d in domains if d["is_primary"])
    return [
        r for r in primary["records"]
        if r["record_type"] == "TLSA" and r["name"] == TLSA_NAME
    ]


# ── Test ──────────────────────────────────────────────────────────────


@pytest.mark.feature("certificates-by-themselves")
def test_dane_tlsa_floor_present_then_trusted_withdraws(dane_nest):
    host, port, name = "127.0.0.1", dane_nest["port"], dane_nest["name"]

    # 1 — Floor ⇒ present, pinned to the actually-served floor leaf's SPKI.
    rows = _floor_mx_tlsa_rows(_list_records(dane_nest))
    assert len(rows) == 1, f"exactly one floor-MX TLSA on the floor, got {rows}"
    rdata = rows[0]["expected"]
    assert rdata.startswith("3 1 1 "), f"DANE-EE/SPKI/SHA-256 record, got {rdata!r}"
    pinned_hex = rdata.split()[-1].lower()

    served_hex = served_leaf_spki_sha256_hex(host, port, MX_SNI)
    assert pinned_hex == served_hex, (
        "the published floor-MX TLSA must pin the SPKI of the leaf the image "
        f"actually serves for {MX_SNI}: published {pinned_hex}, served {served_hex}"
    )

    # 2 — Floor → trusted: land a CA-issued (issuer != subject) leaf at acme_dir.
    fullchain, key = make_ca_issued_leaf_pem([DOMAIN, MX_SNI])
    trusted_served_hex = spki_sha256_hex(x509.load_pem_x509_certificate(fullchain))
    assert trusted_served_hex != pinned_hex, "the flip must change the served SPKI"
    install_acme_default_cert(name, fullchain, key)

    # 3 — Trusted ⇒ withdrawn. Poll past the watcher's 2 s debounce + reload.
    deadline = time.monotonic() + 30
    while True:
        # Confirm the hot-reload actually took before asserting the withdrawal,
        # so a still-floor cert can't masquerade as a withdrawal failure.
        if served_leaf_spki_sha256_hex(host, port, MX_SNI) == trusted_served_hex:
            rows = _floor_mx_tlsa_rows(_list_records(dane_nest))
            if not rows:
                break
        if time.monotonic() > deadline:
            rows = _floor_mx_tlsa_rows(_list_records(dane_nest))
            now_served = served_leaf_spki_sha256_hex(host, port, MX_SNI)
            assert now_served == trusted_served_hex, (
                f"cert-watcher never reloaded the trusted leaf (still serving "
                f"{now_served}, expected {trusted_served_hex})"
            )
            assert not rows, (
                "a trusted MX cert must withdraw the floor-key TLSA, still present: "
                f"{rows}"
            )
        time.sleep(1)

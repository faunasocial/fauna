"""tier_4 e2e: the per-domain **MTA-STS policy-file serving route** + its
**cert-honesty coupling** proven on the real Docker deploy image
(mail-multidomain.md § Policy-file server; tls-certificates.md § D — the last remaining MTA-STS floor-vs-trusted half).

This is the real-HTTPS acceptance the tier_3 conformance test
(``bins/fauna-nest/tests/conformance_mta_sts_serving.rs``) defers to the docker
slice: that test drives ``build_router`` against an in-process
``MultiDomainCertResolver``; here the **packaged image** serves
``/.well-known/mta-sts.txt`` over its own boot-time self-signed floor on a real
HTTPS listener, with ``state.served_cert_spki`` wired through ``main.rs``. Only
this tier exercises the whole serving stack — self-signed bootstrap →
``MultiDomainCertResolver`` → ``AppState.served_cert_spki`` → ``mta_sts_handler``
reading ``mail_domains`` over the live db — that the tier_3 test stubs by
constructing the resolver directly. The Dockerfile builds
``--features bluesky,nostr,activitypub`` (**no** ``test-hooks``), so the floor→trusted
flip below uses the real cert path, not an override.

What it asserts (``tls-certificates.md`` § D — the cert-honesty coupling, served
side):

  1. **Floor ⇒ never enforce.** A primary domain whose MX (``mail.<primary>``)
     is on the self-signed floor serves ``mode: testing`` — never advertising a
     WebPKI trust the floor can't keep (RFC 8461 §5 — an enforcing sender would
     refuse the non-WebPKI MX and bounce inbound mail).
  2. **Floor → trusted, inside the window ⇒ still testing.** Drop a CA-issued
     leaf (issuer DN ≠ subject DN, so ``is_floor == false``) covering the MX into
     the container's ``acme_dir`` — exactly where a real ACME issuance lands the
     renewed chain. The cert-watcher hot-reloads it and the listener presents it;
     the served policy stays ``mode: testing``, because a trusted certificate
     alone never raises the mode: the nest stores every new domain ``testing``
     and advances it to ``enforce`` only after its 7-day window
     (``mail-multidomain.md`` § The advance).

**What this tier can no longer show, and where it is shown.** No request sets
the mode, and the image has no clock to move (none is added for a test), so a
fresh container cannot reach a stored ``enforce``. The advance itself, and the
trusted-certificate ⇒ ``mode: enforce`` serving of an advanced domain, are
proven in-process by ``conformance_mta_sts_serving.rs`` (through the real
``build_router``) and the nest's ``mta_sts_advance`` unit tests.

Mirrors ``test_dane_tlsa_cert_coupling.py`` (the DANE half of the same § D
coupling): same fresh-claimed-floor-nest fixture + ``register_primary_domain`` +
the shared ``make_ca_issued_leaf_pem`` / ``install_acme_default_cert`` flip
helpers (priority #2). The two are the inbound MTA-trust pair — DANE pins the MX
SPKI for DANE senders; MTA-STS gates the published policy mode for MTA-STS
senders — and both withdraw/downgrade the moment the floor lifts.

**Why no fake-DNS / mail-serving precondition.** The served policy mode is
computed from the *locally served* cert facts (``served_cert_facts("mail.<primary>")``)
and the stored ``mail_domains.mta_sts_mode`` — no external DNS lookup, no MTA/MDA
listener. ``register_primary_domain`` reaches the active-primary state the route
reads; the slow bridge-enrollment dance is not needed.
"""

import ssl
import subprocess
import time

import pytest
from cryptography.hazmat.primitives import serialization
from helpers.tls_spki import served_leaf

from .helpers import (
    IMAGE_TAG,
    claim_admin_api,
    docker_build,
    find_free_ports,
    get_repo_root,
    install_acme_default_cert,
    make_ca_issued_leaf_pem,
    register_primary_domain,
    remove_container,
    sni_https_request,
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
CLAIM_CODE = "MTASTS4"
MX_SNI = f"mail.{DOMAIN}"
POLICY_HOST = f"mta-sts.{DOMAIN}"
POLICY_PATH = "/.well-known/mta-sts.txt"


# ── Fixtures ──────────────────────────────────────────────────────────


@pytest.fixture(scope="module")
def docker_image():
    """Build the image once for this module (shared tag, layer-cached)."""
    docker_build(get_repo_root())
    yield IMAGE_TAG


@pytest.fixture()
def mta_sts_nest(docker_image):
    """Fresh claimed nest serving HTTPS on its boot-time self-signed floor, with a
    single primary mail domain (stored ``testing``, as every new domain is). No
    mail listeners — the policy route is cert-coupled, not mail-serving-gated.
    Cleaned up after."""
    (http_port,) = find_free_ports(1)
    name = f"fauna-mtasts-{http_port}"
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


def _served_mode(port: int) -> str:
    """Fetch the served `/.well-known/mta-sts.txt` over TLS with SNI + Host =
    `mta-sts.<domain>` (verify-off — the floor is self-signed) and return the
    parsed `mode:` line. Asserts 200 + a well-formed RFC 8461 §3.2 body."""
    status, _headers, body = sni_https_request(port, POLICY_HOST, "GET", POLICY_PATH)
    text = body.decode()
    assert status == 200, f"policy file should serve 200, got {status}: {text!r}"
    assert "version: STSv1" in text, f"RFC 8461 §3.2 body expected: {text!r}"
    assert f"mx: {MX_SNI}" in text, f"shared mail.<primary> MX target expected: {text!r}"
    modes = [ln.split(":", 1)[1].strip() for ln in text.splitlines() if ln.startswith("mode:")]
    assert len(modes) == 1, f"exactly one mode: line expected, got {text!r}"
    return modes[0]


def _presented_leaf_der(port: int) -> bytes:
    """The DER of the leaf the listener presents for the MX SNI (verify-off)."""
    return served_leaf("127.0.0.1", port, MX_SNI).public_bytes(
        serialization.Encoding.DER
    )


# ── Test ──────────────────────────────────────────────────────────────


@pytest.mark.feature("certificates-by-themselves")
def test_mta_sts_never_enforces_on_the_floor_or_inside_the_window(mta_sts_nest):
    port, name = mta_sts_nest["port"], mta_sts_nest["name"]

    # 1 — Floor ⇒ `testing`, never `enforce`.
    assert _served_mode(port) == "testing", (
        "a domain whose MX is on the self-signed floor must not advertise enforce"
    )

    # 2 — Floor → trusted: land a CA-issued (issuer != subject) leaf at acme_dir
    #     and wait for the listener to present it (the watcher debounces, then
    #     reloads). The wait is on the presented leaf itself, not on a delay.
    fullchain, key = make_ca_issued_leaf_pem([DOMAIN, MX_SNI])
    installed_leaf = ssl.PEM_cert_to_DER_cert(
        fullchain.decode().split("-----END CERTIFICATE-----")[0]
        + "-----END CERTIFICATE-----\n"
    )
    install_acme_default_cert(name, fullchain, key)
    deadline = time.monotonic() + 30
    while _presented_leaf_der(port) != installed_leaf:
        assert time.monotonic() < deadline, (
            "the listener never presented the installed CA-issued leaf for "
            f"{MX_SNI}"
        )
        time.sleep(1)

    # 3 — Trusted, but the domain was added moments ago: still `testing`. A
    #     trusted certificate alone never raises the mode.
    assert _served_mode(port) == "testing", (
        "a domain inside its 7-day testing window must publish testing even on "
        "a trusted MX certificate"
    )

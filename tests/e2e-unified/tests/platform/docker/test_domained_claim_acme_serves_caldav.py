"""tier_4 e2e: **DOMAINLESS boot → claim with a DOMAINED handle
(``admin@<domain>``) → the primary domain is set AT CLAIM → Admin→DNS lists its
records → REAL ACME covers ``mail.<domain>`` → CalDAV answers via the SNI router.**

The sibling ``test_domainless_add_domain_acme_serves_mail.py`` proves the
*bare-handle-then-add-domain* branch of the domainless-boot model. This test
proves the OTHER branch — the one whose absence produced the original
user-visible bug ("claimed with ``admin@<domain>`` but Admin→DNS shows no
records"): the claim handle's ``@domain`` suffix travels as ``mail_domain`` and,
for a real (non-local) domain, becomes the primary ``mail_domains`` row **at
claim** with NO add-domain step (``claim_core`` →
``mail_enable::ensure_mail_domain_registered`` →
``identity_domain_core::apply_primary_identity``) —
``domains-and-tls-bootstrap.md`` § Claim; ``mail-multidomain.md`` § The primary
domain. The tier_3 twin (``tests/api/test_claim_primary_domain.py``) locks the
row + DNS-matrix contract against a spawned binary; only this tier_4 leg proves
it through the real image — entrypoint env plumbing, ACME against a real CA
(pebble), the s6-supervised bridges, and the SNI router in front of CalDAV.

Asserts:
  1. **The primary domain is set at claim.** The claim reply's ``domain``
     follows the handle's ``mail_domain``; ``fauna.bridges.list_local_domains``
     lists exactly that domain as ``is_primary`` — with no
     ``add_local_domain`` call anywhere in this test.
  2. **Admin→DNS lists the primary's records** (``fauna.dns.list_records``
     non-empty, MX + TXT present) — the exact matrix whose emptiness was the
     original bug.
  3. **Real ACME ran on the claimed domain**: the served leaf flips
     floor→pebble-issued, ``fauna.tls.cert_status`` reports
     ``ValidTrusted``/``is_floor == false``, and the issued chain covers
     ``mail.<domain>`` — TLS acquisition triggered by nothing but the claim.
  4. **CalDAV answers through the SNI router**: with the bridges serving, an
     unauthenticated ``PROPFIND /caldav/`` with SNI ``mail.<domain>`` against
     the router's :443 is challenged with the MDA's own ``fauna-caldav`` realm
     (proving the byte stream reached the MDA CalDAV listener), while SNI
     ``<domain>`` on the same port reaches nest health — the SNI split working
     for a domain that did not exist at boot.

Reuses the pebble + fake_dns + trust-bundle scaffolding of
``test_acme_http01_pebble_issuance.py`` (imported, not duplicated) and the SNI
probe of ``test_caldav_sni_router.py`` (``helpers.sni_https_request``).
"""

import json
import os
import subprocess
import tempfile
import time

import pytest
from cryptography import x509


from helpers.tls_spki import served_leaf

from .helpers import (
    admin_ws,
    claim_admin_api,
    container_ip,
    create_network,
    find_free_ports,
    get_repo_root,
    remove_container,
    remove_network,
    sni_https_request,
    start_container_with_ports,
    start_fake_dns_sidecar,
    start_fake_scanner_sidecars,
    wait_for_health,
    write_dns_records,
    bring_bridges_to_serving,
)

# Reuse the pebble lifecycle + cert-read scaffolding from the sibling ACME test
# rather than duplicating it (same note as the add-domain sibling: test-infra
# helpers; if a fourth pebble test appears, lift them to helpers.py).
from .test_acme_http01_pebble_issuance import (
    PEBBLE_CONFIG,
    _cert_status,
    _extract_pebble_ca,
    _image_ca_bundle,
    _start_pebble,
    _wait_tcp,
)

try:
    subprocess.run(["docker", "info"], capture_output=True, timeout=10)
    HAS_DOCKER = True
except Exception:
    HAS_DOCKER = False

pytestmark = [
    pytest.mark.skipif(not HAS_DOCKER, reason="Docker not available"),
    pytest.mark.tier_4,
]

# The domain the admin claims WITH (the handle is `admin@<this>`). A
# real-looking, non-local domain: pebble refuses `localhost`, and the
# claim-time registration is gated to `is_public_dns_name` targets.
CLAIMED_DOMAIN = "claimed.test"
MX_SNI = f"mail.{CLAIMED_DOMAIN}"
CLAIM_CODE = "DMCLM1"


def _leaf_is_pebble(leaf: x509.Certificate) -> bool:
    return "Pebble" in leaf.issuer.rfc4514_string()


@pytest.fixture()
def domained_claim_stack():
    """A docker network with `pebble` (real ACME CA) + `fake_dns` + fake
    clamd/rspamd scanners + a **domainless** nest (NO ``FAUNA_DOMAIN``) whose
    admin then claims with the DOMAINED handle ``admin@<domain>``. The claim is
    the only domain-introducing action. The router's :443 is mapped so the
    CalDAV/SNI leg can be probed. Yields ``(nest, mail_ports)``."""
    net = f"fauna-dmclaim-net-{os.getpid()}"
    dns_name = f"fauna-dmclaim-dns-{os.getpid()}"
    pebble_name = "pebble"  # must match the pebble directory-cert SAN + the URL
    clamd_name = f"fauna-dmclaim-clamd-{os.getpid()}"
    rspamd_name = f"fauna-dmclaim-rspamd-{os.getpid()}"
    (api_port, router_port, pebble_host_port, p25, p465, p587, p993) = find_free_ports(7)
    nest_name = f"fauna-dmclaim-nest-{api_port}"
    mail_ports = {25: p25, 465: p465, 587: p587, 993: p993}

    workdir = tempfile.mkdtemp(prefix="fauna-dmclaim-")
    records_dir = os.path.join(workdir, "dns")
    os.makedirs(records_dir, exist_ok=True)

    started = []
    try:
        create_network(net)

        fakes_dir = str(get_repo_root() / "tests" / "e2e-unified" / "fakes")
        dns_ip = start_fake_dns_sidecar(
            net, fakes_dir, name=dns_name, txt_records={}, records_dir=records_dir
        )
        started.append(dns_name)

        scan = start_fake_scanner_sidecars(
            net, fakes_dir, clamd_name=clamd_name, rspamd_name=rspamd_name
        )
        started.append(clamd_name)
        started.append(rspamd_name)

        config_path = os.path.join(workdir, "pebble-config.json")
        with open(config_path, "w", encoding="utf-8") as f:
            json.dump(PEBBLE_CONFIG, f)
        _start_pebble(net, pebble_name, config_path, f"{dns_ip}:53", pebble_host_port)
        started.append(pebble_name)
        _wait_tcp("127.0.0.1", pebble_host_port, timeout=30)
        pebble_ca = _extract_pebble_ca(pebble_name)

        bundle_path = os.path.join(workdir, "ssl-bundle.pem")
        with open(bundle_path, "wb") as f:
            f.write(_image_ca_bundle())
            f.write(b"\n")
            f.write(pebble_ca)
        os.chmod(bundle_path, 0o644)

        start_container_with_ports(
            nest_name,
            {3000: api_port, 443: router_port, **mail_ports},
            env={
                # NO FAUNA_DOMAIN — the box boots domainless; the CLAIM (not an
                # env var, not an add-domain call) introduces the domain.
                "FAUNA_CLAIM_CODE": CLAIM_CODE,
                "FAUNA_PORT": "3000",
                "FAUNA_MODE": "public",
                "FAUNA_ACME_DIRECTORY_URL": f"https://{pebble_name}:14000/dir",
                "SSL_CERT_FILE": "/etc/fauna/ssl-bundle.pem",
                "FAUNA_CLAMD_ADDR": scan["clamd_addr"],
                "FAUNA_RSPAMD_URL": scan["rspamd_url"],
            },
            network=net,
            mounts=[(bundle_path, "/etc/fauna/ssl-bundle.pem")],
        )
        started.append(nest_name)

        # Publish the claimed domain's A records → the nest IP BEFORE the claim,
        # so the ACME order the claim triggers can validate immediately (pebble
        # resolves via fake_dns and reaches the nest's HTTP-01 listener). Both
        # the apex and `mail.<domain>` are in the order's SAN set.
        nest_ip = container_ip(nest_name)
        write_dns_records(records_dir, a={CLAIMED_DOMAIN: nest_ip, MX_SNI: nest_ip})

        wait_for_health(api_port, nest_name)
        # THE ACTION UNDER TEST: claim with a domained handle. `mail_domain`
        # carries the handle's `@domain` exactly as the wizard forwards it.
        admin = claim_admin_api(
            api_port, CLAIM_CODE, handle="admin", mail_domain=CLAIMED_DOMAIN
        )
        url = f"https://127.0.0.1:{api_port}"
        nest = {
            "name": nest_name,
            "port": api_port,
            "router_port": router_port,
            "url": url,
            "admin": admin,
        }
        yield (nest, mail_ports)
    finally:
        for name in reversed(started):
            remove_container(name)
        remove_network(net)
        subprocess.run(["rm", "-rf", workdir], capture_output=True, timeout=10)


@pytest.mark.feature("calendar-in-standard-apps")
@pytest.mark.feature("certificates-by-themselves")
def test_domained_claim_sets_primary_dns_acme_and_caldav(domained_claim_stack):
    nest, mail_ports = domained_claim_stack
    host, port = "127.0.0.1", nest["port"]

    # 1. The primary mail domain was set AT CLAIM — no add-domain step ran.
    assert nest["admin"]["domain"] == CLAIMED_DOMAIN, (
        "the claim reply's domain must follow the handle's mail_domain "
        f"(identity set at claim); got {nest['admin']['domain']!r}"
    )
    with admin_ws(nest) as admin:
        listed = admin.call("fauna.bridges.list_local_domains", {})
        active = listed.get("active", [])
        names = [d["domain_name"] for d in active]
        assert names == [CLAIMED_DOMAIN], (
            f"the claim must register exactly [{CLAIMED_DOMAIN}] as the active "
            f"mail domain; got {names!r}"
        )
        assert active[0]["is_primary"] is True, active[0]

        # 2. Admin→DNS lists the primary's records (emptiness was the original
        #    user-visible bug this branch of the claim path fixed).
        dns = admin.call("fauna.dns.list_records", {})
        domains = dns.get("domains", [])
        assert domains, f"Admin→DNS matrix must be non-empty after a domained claim; got {dns!r}"
        matrix = next(d for d in domains if d["domain"] == CLAIMED_DOMAIN)
        assert matrix["records"], f"no DNS records for the claimed primary: {matrix!r}"
        types = {r["record_type"] for r in matrix["records"]}
        assert "MX" in types and "TXT" in types, (
            f"Admin→DNS for the claimed primary must include MX and SPF/DMARC TXT "
            f"rows; got {sorted(types)!r}"
        )

    # 3. Real ACME ran, triggered by nothing but the claim: served leaf flips
    #    floor→pebble-issued, cert-status ValidTrusted, chain covers mail.<domain>.
    deadline = time.monotonic() + 180
    last = ""
    while True:
        leaf = served_leaf(host, port, CLAIMED_DOMAIN)
        pebble_issued = _leaf_is_pebble(leaf)
        status = _cert_status(nest, [CLAIMED_DOMAIN])[CLAIMED_DOMAIN]
        trusted = (status["state"] == "ValidTrusted") and (status["is_floor"] is False)
        sans: list[str] = []
        try:
            ext = leaf.extensions.get_extension_for_class(x509.SubjectAlternativeName)
            sans = ext.value.get_values_for_type(x509.DNSName)
        except x509.ExtensionNotFound:
            sans = []
        covers_mail = MX_SNI in sans
        if pebble_issued and trusted and covers_mail:
            break
        last = (
            f"pebble_issued={pebble_issued} cert_status={status} "
            f"covers_mail={covers_mail} sans={sans}"
        )
        if time.monotonic() > deadline:
            logs = subprocess.run(
                ["docker", "logs", "--tail", "80", nest["name"]],
                capture_output=True, text=True, timeout=15,
            )
            raise AssertionError(
                "the domained claim did not lead to a trusted ACME cert covering "
                f"mail.<domain> in time. last={last}\n"
                f"--- nest logs (tail) ---\n{logs.stdout}\n{logs.stderr}"
            )
        time.sleep(2)

    # 4. CalDAV answers through the SNI router for the claimed domain. Bring the
    #    bridges to serving (the claim-registered primary satisfies its
    #    registered-domain precondition — that registration happening at claim
    #    is the point of this test), then probe the router's :443.
    bring_bridges_to_serving(nest["name"], nest, mail_ports, CLAIMED_DOMAIN)

    # SNI <domain> → nest (router default backend answers health).
    status_code, _headers, body = sni_https_request(
        nest["router_port"], CLAIMED_DOMAIN, "GET", "/api/v1/health")
    assert status_code == 200, (
        f"SNI {CLAIMED_DOMAIN!r} on the router :443 must reach nest health; "
        f"got {status_code}, body={body[:200]!r}"
    )

    # SNI mail.<domain> → the MDA CalDAV listener: the unauthenticated PROPFIND
    # is challenged with the MDA's OWN realm (nest never emits `fauna-caldav`),
    # proving the router routed the claimed domain's mail SNI to CalDAV. The MDA
    # builds its TLS provider from the claim-registered domain's cert fan-out,
    # so a handshake succeeding here is itself part of the proof. Allow a short
    # settle for the router/MDA to pick up the listener.
    deadline = time.monotonic() + 60
    while True:
        try:
            status_code, headers, body = sni_https_request(
                nest["router_port"], MX_SNI, "PROPFIND", "/caldav/")
            if status_code == 401:
                break
        except OSError as e:
            status_code, headers, body = None, {}, repr(e).encode()
        if time.monotonic() > deadline:
            raise AssertionError(
                f"SNI {MX_SNI} PROPFIND /caldav/ over the router :443 never got the "
                f"CalDAV 401 challenge; last={status_code}, body={body[:200]!r}"
            )
        time.sleep(2)
    www_auth = headers.get("www-authenticate", "")
    assert "fauna-caldav" in www_auth, (
        "the 401 must carry the MDA's WWW-Authenticate realm (proves the MDA "
        f"CalDAV listener answered, not nest); got {www_auth!r}"
    )

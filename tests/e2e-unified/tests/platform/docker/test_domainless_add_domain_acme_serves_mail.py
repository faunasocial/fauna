"""tier_4 e2e: **DOMAINLESS boot → claim-by-handle → add a domain → REAL ACME
issues a trusted cert for the added domain → mail serves on it.**

This is the coverage the ``FAUNA_DOMAIN``-seeded tier_4 tests can't give: they boot
*with* a domain, so the ACME machinery is up from the very first boot. The
recommended out-of-the-box deploy is instead **domainless** — boot with NO
``FAUNA_DOMAIN``, serve only the always-live self-signed floor, claim by a bare
handle, and add the domain later from a client
(``domains-and-tls-bootstrap.md`` § Env contract; ``installers/docker.md``
§ Default: Domainless). This proves that path end-to-end through the real image.

**Status — XFAIL(strict=False), FIX LANDED, tier_4 re-run pending.** The fix closed
the nest *acquisition* half (asserts 1–3: the nest gets a trusted cert for the
post-claim-added domain, chain covers ``mail.<domain>``); the remaining *serving* half —
assert 4, the MDA/IMAPS-993 listener serving that pebble chain — was the MDA-cert-fan-out
gap, and it is now **fixed on ``origin/main``**
("prompt-refresh-on-provision: bridges re-fetch TLS on config_changed"): nest fires a
``config_changed``/``"tls"`` push whenever a cert lands and both bridge roles re-run
``fetch_tls_cert_blob`` on it, so IMAPS/993 swaps its self-signed ``CN=added.test`` leaf
for the pebble chain within seconds. The fix is validated in-process (Rust units) + in Go
(``internal/tls TestConfigReloaderTLSApplierRefetchesCert``); the marker STAYS
``strict=False`` only until the deployer confirms assert 4
green on a fresh image built from ``origin/main`` — then flip to live (see the marker
comment below).**
The nest-side domainless boot (drop ``FAUNA_DOMAIN``), claim-authoritative identity,
and always-spawn ACME machinery landed; it
wired ACME issuance to the **identity apex** (``handle_domain()``), which the *claim*
sets (``claim_core`` → ``mail_enable::ensure_mail_domain_registered`` →
``identity_domain_core::apply_primary_identity``). This test exercises the *other*
canonical domainless flow — reach by IP, claim by a **bare handle** (no domain), then
add the first real domain via Admin→DNS (``fauna.bridges.add_local_domain``). The gap
was that ``add_local_domain_handler`` wrote the primary ``mail_domains`` row
**directly** (``add_mail_domain``) without calling ``apply_primary_identity``, so the
identity apex stayed ``localhost`` → the cert-lifecycle's ``desired`` SAN set was empty
→ no cert for the added domain (contradicting ``domains-and-tls-bootstrap.md`` § Claim,
":98-100"). **Fixed on ``origin/main``:**
``add_local_domain_handler`` now calls ``identity_domain_core::apply_primary_identity``
when the added row is the first (primary) domain and a real (non-``is_local``) target,
so ``handle_domain()`` follows the added domain and the lifecycle orders + issues its
cert — and this is confirmed: executed on an image on
2026-07-02, asserts 1–3 PASS. The mail-serving leg (assert 4) — the MDA/IMAPS-993 listener
serving the pebble chain for ``mail.<domain>`` — was the remaining, narrower gap (nest cert
*acquisition* worked; nest→MDA cert *serving* on a running bridge did not: the MDA
re-fetches its TLS blob only on a 12 h timer, so the just-issued trusted cert sat unserved
and IMAPS/993 kept its self-signed ``CN=added.test`` leaf). **That is now fixed on
``origin/main`` ("prompt-refresh-on-provision: bridges re-fetch
TLS on config_changed"):** nest fires a ``config_changed``/``"tls"`` push whenever a cert
lands (``cert_lifecycle_loop`` after an ACME issue; ``provision_self_signed_cert``, which
also wakes ``acme_retry_notify`` so an ACME-on box's interim self-signed re-heals to trusted
at once) and both bridge roles register a ``configReloader`` applier that re-runs
``fetch_tls_cert_blob`` on it (seal-on-read hands back the cert on disk), so 993 swaps to the
pebble chain within seconds. Validated in-process (Rust units:
``bridge_routing_handlers::tests::add_local_domain_creates_primary_then_lists``) + in Go
(``internal/tls TestConfigReloaderTLSApplierRefetchesCert``); the ``strict=False`` marker
stays only until the deployer confirms assert 4 green on a
fresh image built from ``origin/main`` (see the marker comment below).

(The claim-with-a-real-domain path — ``alice@example.com`` → identity set at claim →
ACME issues — already works and is covered by its
tier_3 test; this test covers the bare-handle-then-add-domain path it did
not.)

Asserts:
  1. **Domainless boot serves only the floor.** After claim-by-handle (no
     ``mail_domain``), the nest's HTTPS listener presents a **self-signed** leaf
     (issuer is NOT pebble) — the box has no CA cert and no registered domain.
  2. **Add the domain** (``fauna.bridges.add_local_domain``) + publish its
     fake-DNS ``A`` record → the nest's container IP.
  3. **Real ACME ran on the added domain.** The served leaf for the added domain
     flips floor→**pebble-issued**, ``fauna.tls.cert_status`` reports it
     ``ValidTrusted`` / ``is_floor == false``, and the issued chain **covers
     ``mail.<domain>``** (the mail hostname the MDA serves) — the exact behavior
     the fix enables and that was absent before.
  4. **Mail serves on the added domain with that trusted cert.** The bridges come
     to serving and the IMAPS (993) listener presents the **pebble-issued** leaf
     for ``mail.<domain>`` — mail TLS on a domain that did not exist at boot.

Reuses the pebble + fake_dns + trust-bundle scaffolding of
``test_acme_http01_pebble_issuance.py`` (imported, not duplicated).
"""

import os
import subprocess
import tempfile
import time

import pytest
from cryptography import x509


from helpers.tls_spki import served_leaf

from .helpers import (
    admin_ws,
    bridge_diag,
    bring_bridges_to_serving,
    claim_admin_api,
    container_ip,
    create_network,
    find_free_ports,
    get_repo_root,
    register_primary_domain,
    remove_container,
    remove_network,
    start_container_with_ports,
    start_fake_dns_sidecar,
    start_fake_scanner_sidecars,
    wait_for_health,
    write_dns_records,
)

# Reuse the pebble lifecycle + cert-read scaffolding from the sibling ACME test
# rather than duplicating ~60 lines of it (priority #2). These are test-infra
# helpers, not production code; if a third pebble test appears, lift them to
# helpers.py.
from .test_acme_http01_pebble_issuance import (
    NEST_HTTP01_PORT,
    PEBBLE_CONFIG,
    _cert_status,
    _extract_pebble_ca,
    _image_ca_bundle,
    _start_pebble,
    _wait_tcp,
)

pytestmark = pytest.mark.tier_4

try:
    subprocess.run(["docker", "info"], capture_output=True, timeout=10)
    HAS_DOCKER = True
except Exception:
    HAS_DOCKER = False

pytestmark = [
    pytest.mark.skipif(not HAS_DOCKER, reason="Docker not available"),
    pytest.mark.tier_4,
    # LIVE since 2026-07-10: assert 4 XPASSED on image `sha-dd9dbd60f…` (the
    # first deployer build ≥ BOTH halves of the fix — the nest setup's
    # `config_changed`/`"tls"` push + bridge re-fetch, and the ACME
    # reused-authorization skip that let the self-heal actually
    # re-land a trusted cert inside the CA reuse window), so the long-standing
    # xfail(strict=False) marker was deleted per its own flip-to-live protocol.
]

# A real-looking domain the client ADDS after claiming a domainless box (pebble
# refuses `localhost`; the nest orders only for a real, non-`localhost` domain).
# NOT passed as FAUNA_DOMAIN — the box boots domainless and this is added later.
ADDED_DOMAIN = "added.test"
MX_SNI = f"mail.{ADDED_DOMAIN}"
CLAIM_CODE = "DMLESS1"


def _leaf_is_pebble(leaf: x509.Certificate) -> bool:
    return "Pebble" in leaf.issuer.rfc4514_string()


@pytest.fixture()
def domainless_stack():
    """A docker network with `pebble` (real ACME CA) + `fake_dns` + fake
    clamd/rspamd scanners + a **domainless** nest (NO ``FAUNA_DOMAIN``) pointed at
    pebble via ``FAUNA_ACME_DIRECTORY_URL`` and trusting its directory CA via
    ``SSL_CERT_FILE``. The nest boots on the floor with the ACME machinery up but
    idle. Yields ``(nest, mail_ports)``; tears the whole stack down after."""
    net = f"fauna-dmless-net-{os.getpid()}"
    dns_name = f"fauna-dmless-dns-{os.getpid()}"
    pebble_name = "pebble"  # must match the pebble directory-cert SAN + the URL
    clamd_name = f"fauna-dmless-clamd-{os.getpid()}"
    rspamd_name = f"fauna-dmless-rspamd-{os.getpid()}"
    (api_port, pebble_host_port, p25, p465, p587, p993) = find_free_ports(6)
    nest_name = f"fauna-dmless-nest-{api_port}"
    mail_ports = {25: p25, 465: p465, 587: p587, 993: p993}

    workdir = tempfile.mkdtemp(prefix="fauna-dmless-")
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
            import json

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
            {3000: api_port, **mail_ports},
            env={
                # NO FAUNA_DOMAIN — this is the domainless boot under test.
                "FAUNA_CLAIM_CODE": CLAIM_CODE,
                "FAUNA_PORT": "3000",
                "FAUNA_MODE": "public",
                # Point the domainless box's ACME client at pebble. This is what the
                # entrypoint plumbs into `[acme]` (directory_url) so the machinery —
                # spawned regardless of a boot domain — orders against pebble once a
                # domain is added.
                "FAUNA_ACME_DIRECTORY_URL": f"https://{pebble_name}:14000/dir",
                "SSL_CERT_FILE": "/etc/fauna/ssl-bundle.pem",
                "FAUNA_CLAMD_ADDR": scan["clamd_addr"],
                "FAUNA_RSPAMD_URL": scan["rspamd_url"],
            },
            network=net,
            mounts=[(bundle_path, "/etc/fauna/ssl-bundle.pem")],
        )
        started.append(nest_name)

        # Publish the added domain's A records → the nest IP so pebble's HTTP-01
        # validation (once the domain is added) resolves them and reaches the
        # nest's :8080 challenge listener. Both apex-name and `mail.<domain>` are
        # covered (the order's SAN set is `[<domain>, mail.<domain>]`).
        nest_ip = container_ip(nest_name)
        write_dns_records(records_dir, a={ADDED_DOMAIN: nest_ip, MX_SNI: nest_ip})

        wait_for_health(api_port, nest_name)
        admin = claim_admin_api(api_port, CLAIM_CODE, handle="admin")  # bare handle
        url = f"https://127.0.0.1:{api_port}"
        nest = {"name": nest_name, "port": api_port, "url": url, "admin": admin}
        yield (nest, mail_ports)
    finally:
        for name in reversed(started):
            remove_container(name)
        remove_network(net)
        subprocess.run(["rm", "-rf", workdir], capture_output=True, timeout=10)


def _active_mail_domains(nest) -> list[dict]:
    with admin_ws(nest) as admin:
        return admin.call("fauna.bridges.list_local_domains", {}).get("domains", [])


@pytest.mark.feature("admin-dns-and-certificates", "certificates-by-themselves")
def test_domainless_add_domain_acquires_acme_cert_and_serves_mail(domainless_stack):
    nest, mail_ports = domainless_stack
    host, port = "127.0.0.1", nest["port"]

    # 1. Domainless boot serves only the self-signed floor — no CA cert, and the
    #    box has no registered mail domain yet.
    assert not _active_mail_domains(nest), (
        "a domainless box must have NO mail domain registered at claim-by-handle"
    )
    floor_leaf = served_leaf(host, port, ADDED_DOMAIN)
    assert not _leaf_is_pebble(floor_leaf), (
        "a domainless box must serve the self-signed floor (not a CA cert) before "
        "any domain is added"
    )

    # 2. Add the domain from the (admin) client — the post-claim affordance.
    register_primary_domain(nest, ADDED_DOMAIN)

    # 3. Real ACME ran on the ADDED domain: the served leaf flips
    #    floor→pebble-issued, the cert-status badge flips to ValidTrusted, and the
    #    issued chain covers `mail.<domain>`. This is the gap-closure proof — a
    #    domainless box acquiring a trusted cert for a domain added post-claim.
    deadline = time.monotonic() + 180
    last = ""
    while True:
        leaf = served_leaf(host, port, ADDED_DOMAIN)
        pebble_issued = _leaf_is_pebble(leaf)
        status = _cert_status(nest, [ADDED_DOMAIN])[ADDED_DOMAIN]
        trusted = (status["state"] == "ValidTrusted") and (status["is_floor"] is False)
        # The served leaf must cover mail.<domain> too (the SAN the MDA serves).
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
                "a domainless box did not acquire a trusted ACME cert for the "
                f"domain added post-claim in time. last={last}\n"
                f"--- nest logs (tail) ---\n{logs.stdout}\n{logs.stderr}"
            )
        time.sleep(2)

    # 4. Mail serves on the added domain with that trusted cert: bring the bridges
    #    to serving and confirm the IMAPS (993) listener presents the pebble-issued
    #    leaf for `mail.<domain>` — mail TLS on a domain that did not exist at boot.
    #    (register_primary_domain above satisfies bring_bridges_to_serving's
    #    precondition; it is idempotent on an already-active domain.)
    bring_bridges_to_serving(nest["name"], nest, mail_ports, ADDED_DOMAIN)

    # The cert-watcher fans the ACME chain out to the MDA (sealed); poll the served
    # IMAPS leaf until it is the pebble-issued cert for mail.<domain> (allowing the
    # seal-and-fan-out + any interim self-signed to self-heal).
    deadline = time.monotonic() + 90
    last_issuer = ""
    while True:
        mail_leaf = served_leaf(host, mail_ports[993], MX_SNI)
        if _leaf_is_pebble(mail_leaf):
            break
        last_issuer = mail_leaf.issuer.rfc4514_string()
        if time.monotonic() > deadline:
            # DIAGNOSTIC: distinguish a nest-side re-heal failure (the nest's own
            # HTTPS on `port` is ALSO self-signed → the trusted cert never got
            # re-written to disk after the provision_self_signed clobber) from an
            # MDA-fetch failure (nest HTTPS trusted but 993 stuck self-signed →
            # the config_changed push / seal-on-read never reached the listener).
            api_leaf = served_leaf(host, port, ADDED_DOMAIN)
            api_issuer = api_leaf.issuer.rfc4514_string()
            api_is_pebble = _leaf_is_pebble(api_leaf)
            status = _cert_status(nest, [ADDED_DOMAIN]).get(ADDED_DOMAIN)
            logs = subprocess.run(
                ["docker", "logs", "--tail", "260", nest["name"]],
                capture_output=True, text=True, timeout=15,
            )
            keep = ("acme", "tls", "config_changed", "config changed",
                    "issu", "self-heal", "self-signed", "cert", "refresh",
                    "seal", "provision", "retry", "budget", "fetch_tls")
            nest_lines = [
                ln for ln in (logs.stdout + logs.stderr).splitlines()
                if any(k in ln.lower() for k in keep)
            ]
            raise AssertionError(
                "the IMAPS (993) listener never served the pebble-issued cert for "
                f"{MX_SNI}; last issuer={last_issuer!r}\n"
                "--- DIAGNOSTIC (nest re-heal vs MDA fetch) ---\n"
                f"nest HTTPS (port {port}) leaf issuer={api_issuer!r} "
                f"is_pebble={api_is_pebble}\n"
                f"cert_status[{ADDED_DOMAIN}]={status}\n"
                f"--- bridge_diag ---\n{bridge_diag(nest['name'])}\n"
                "--- nest acme/tls log lines (tail) ---\n"
                + "\n".join(nest_lines[-60:])
            )
        time.sleep(2)

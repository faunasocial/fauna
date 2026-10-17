"""tier_4 e2e: **DOMAINLESS boot → claim-by-handle → add a domain → REAL ACME
issues a trusted cert for the added domain → mail serves on it.**

This is the coverage the ``FAUNA_DOMAIN``-seeded tier_4 tests can't give: they boot
*with* a domain, so the ACME machinery is up from the very first boot. The
recommended out-of-the-box deploy is instead **domainless** — boot with NO
``FAUNA_DOMAIN``, serve only the always-live self-signed floor, claim by a bare
handle, and add the domain later from a client
(``domains-and-tls-bootstrap.md`` § Env contract; ``installers/docker.md``
§ Default: Domainless). This proves that path end-to-end through the real image.

**Status — LIVE (see the marker comment below); the history follows.** The fix closed
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

Three later tests run on the same box (the stack is module-scoped): mail from an
outside sender reaches a member on the added domain (`admin-dns-and-certificates`
outcome 65); a second domain that does not point here yet never breaks the
primary's certificate, then joins it once it does (outcome 57 — and
`certificates-by-themselves` outcome 8's re-issue when the served names change);
and an admin's self-signed stopgap heals back to a new trusted certificate that
reaches the mail and calendar ports with no restart (`certificates-by-themselves`
outcomes 6 and 12; the SNI router's 443 is published for the calendar half).

Reuses the pebble + fake_dns + trust-bundle scaffolding of
``test_acme_http01_pebble_issuance.py`` (imported, not duplicated).
"""

import os
import re
import subprocess
import tempfile
import time

import pytest
from cryptography import x509


from helpers.tls_spki import served_leaf
from helpers.waiting import wait_until

from .helpers import (
    admin_ws,
    bridge_diag,
    bring_bridges_to_serving,
    claim_admin_api,
    container_ip,
    create_network,
    deliver_inbound_loopback_curl,
    enforce_mail_perimeter,
    find_free_ports,
    get_repo_root,
    imap_fetch_only_inbox_message,
    provision_mail_recipient,
    provision_self_signed_cert,
    register_primary_domain,
    remove_container,
    remove_network,
    start_container_with_ports,
    start_fake_dns_sidecar,
    start_fake_scanner_sidecars,
    svstat,
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

# The external sender of the inbound message (outcome 65): delivered over the
# container's loopback, so its published SPF authorizes 127.0.0.1, and it carries
# a `p=reject` DMARC policy the enforced perimeter checks for real.
SENDER_DOMAIN = "sender.test"
RECIPIENT_LOCAL = "reader"
RECIPIENT_PASSWORD = "domainless-reader-pw"

# A second domain the admin adds once the primary's certificate is serving
# (outcome 57). It does not resolve until the test publishes its record.
SECOND_DOMAIN = "second.test"

# The nest's line each cycle it leaves a secondary out of the order because its
# name does not point here yet (`acme_http01::reachable_mail_domains`).
SECONDARY_DEFERRED_LOG = "secondary domain apex does not resolve to the nest yet"


def _publish_dns(records_dir: str, nest_ip: str, *, second: bool = False) -> None:
    """Publish the stack's whole zone in ONE write (the fake name server reloads
    the file whole, so a second write would drop the first): the added domain and
    its mail host at the nest, the sender's passing SPF + DMARC policy, and — once
    ``second`` — the second domain at the nest too."""
    a = {ADDED_DOMAIN: nest_ip, MX_SNI: nest_ip, SENDER_DOMAIN: "127.0.0.1"}
    if second:
        a[SECOND_DOMAIN] = nest_ip
    write_dns_records(
        records_dir,
        a=a,
        mx={SENDER_DOMAIN: f"0 {SENDER_DOMAIN}"},
        txt={
            SENDER_DOMAIN: "v=spf1 ip4:127.0.0.1 -all",
            f"_dmarc.{SENDER_DOMAIN}": "v=DMARC1; p=reject",
        },
    )


def _leaf_is_pebble(leaf: x509.Certificate) -> bool:
    return "Pebble" in leaf.issuer.rfc4514_string()


@pytest.fixture(scope="module")
def domainless_stack():
    """A docker network with `pebble` (real ACME CA) + `fake_dns` + fake
    clamd/rspamd scanners + a **domainless** nest (NO ``FAUNA_DOMAIN``) pointed at
    pebble via ``FAUNA_ACME_DIRECTORY_URL`` and trusting its directory CA via
    ``SSL_CERT_FILE``. The nest boots on the floor with the ACME machinery up but
    idle, and resolves through the same ``fake_dns`` pebble does, so what the test
    publishes there is what both the CA and the nest see. Yields
    ``(nest, mail_ports)``; tears the whole stack down after.

    Module-scoped: the tests below run in file order on ONE box, each starting
    where the one before it left the box (the stack costs a full image boot plus
    an ACME issuance)."""
    net = f"fauna-dmless-net-{os.getpid()}"
    dns_name = f"fauna-dmless-dns-{os.getpid()}"
    pebble_name = "pebble"  # must match the pebble directory-cert SAN + the URL
    clamd_name = f"fauna-dmless-clamd-{os.getpid()}"
    rspamd_name = f"fauna-dmless-rspamd-{os.getpid()}"
    (api_port, router_port, pebble_host_port, p25, p465, p587, p993) = find_free_ports(7)
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
            {3000: api_port, 443: router_port, **mail_ports},
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
            dns=dns_ip,
            mounts=[(bundle_path, "/etc/fauna/ssl-bundle.pem")],
        )
        started.append(nest_name)

        # Publish the added domain's A records → the nest IP so pebble's HTTP-01
        # validation (once the domain is added) resolves them and reaches the
        # nest's :8080 challenge listener. Both apex-name and `mail.<domain>` are
        # covered (the order's SAN set is `[<domain>, mail.<domain>]`).
        nest_ip = container_ip(nest_name)
        _publish_dns(records_dir, nest_ip)

        wait_for_health(api_port, nest_name)
        admin = claim_admin_api(api_port, CLAIM_CODE, handle="admin")  # bare handle
        url = f"https://127.0.0.1:{api_port}"
        nest = {"name": nest_name, "port": api_port, "url": url, "admin": admin,
                "ip": nest_ip, "records_dir": records_dir, "router_port": router_port}
        yield (nest, mail_ports)
    finally:
        for name in reversed(started):
            remove_container(name)
        remove_network(net)
        subprocess.run(["rm", "-rf", workdir], capture_output=True, timeout=10)


def _active_mail_domains(nest) -> list[dict]:
    with admin_ws(nest) as admin:
        return admin.call("fauna.bridges.list_local_domains", {})["active"]


@pytest.mark.feature("admin-dns-and-certificates", "certificates-by-themselves")
def test_domainless_add_domain_acquires_acme_cert_and_serves_mail(domainless_stack,
                                                                 run_seal_helper):
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
    #    Before the bring-up, put the box in the live-box inbound posture and
    #    provision a reader on the added domain the way a member's app would, so
    #    the mail leg below (outcome 65) runs against the box exactly as it serves.
    enforce_mail_perimeter(nest, domain=ADDED_DOMAIN)
    nest["recipient"] = provision_mail_recipient(
        nest, run_seal_helper, domain=ADDED_DOMAIN, local_part=RECIPIENT_LOCAL,
        password=RECIPIENT_PASSWORD,
    )
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


@pytest.mark.feature("admin-dns-and-certificates")
def test_a_domain_added_after_install_receives_mail_once_its_certificate_is_in_place(
        domainless_stack):
    """Outcome 65: on the box the test above left — a domain added after install,
    its trusted certificate issued and serving on the mail port — a message from
    an outside sender to a member's address on that domain is accepted under the
    enforced perimeter and read back over IMAPS (`domains-and-tls-bootstrap.md`
    § Add domains after install)."""
    nest, mail_ports = domainless_stack
    recipient = nest.get("recipient")
    assert recipient, (
        "the premise: the test above provisions a reader on the added domain and "
        "brings mail to serving — it did not get that far"
    )
    assert recipient["username"].endswith(f"@{ADDED_DOMAIN}"), recipient["username"]
    assert _leaf_is_pebble(served_leaf("127.0.0.1", mail_ports[993], MX_SNI)), (
        "the premise: the mail port serves the added domain's trusted certificate"
    )

    subject = "Mail to a domain added after install"
    body_marker = "domainless add-domain inbound"
    msg_id = deliver_inbound_loopback_curl(
        nest["name"],
        mail_from=f"external-sender@{SENDER_DOMAIN}",
        rcpt_to=recipient["username"],
        subject=subject,
        body_text=f"Hello from the {body_marker} test.\n",
    )
    try:
        raw = imap_fetch_only_inbox_message(
            "127.0.0.1", mail_ports[993], MX_SNI,
            recipient["username"], recipient["password"],
        )
    except (AssertionError, TimeoutError) as e:
        raise AssertionError(f"{e}\n\n── bridge diagnostics ──\n{bridge_diag(nest['name'])}") from e

    text = raw.decode("utf-8", errors="replace")
    assert subject in text, f"the delivered message must carry its Subject; got {text[:400]!r}"
    assert body_marker in text, f"the delivered message must carry its text; got {text[:400]!r}"
    assert msg_id.strip("<>") in text, "the delivered message must carry its Message-ID"


def _sans(leaf: x509.Certificate) -> list[str]:
    try:
        ext = leaf.extensions.get_extension_for_class(x509.SubjectAlternativeName)
    except x509.ExtensionNotFound:
        return []
    return ext.value.get_values_for_type(x509.DNSName)


def _deferrals_of_second(name: str) -> int:
    """How many issuance cycles so far left the second domain out of the order
    because its name does not point here."""
    logs = subprocess.run(["docker", "logs", name], capture_output=True, text=True, timeout=30)
    return sum(
        1 for ln in (logs.stdout + logs.stderr).splitlines()
        if SECONDARY_DEFERRED_LOG in ln and SECOND_DOMAIN in ln
    )


def _issuance_now(nest) -> None:
    """The admin's "retry issuance now" — it wakes the issuance cycle instead of
    waiting out its five-minute poll, and changes nothing else on a box whose
    certificate was never overridden (no preserved backup to restore)."""
    with admin_ws(nest) as admin:
        admin.call("fauna.bridges.restore_real_tls_cert", {})


@pytest.mark.feature("admin-dns-and-certificates", "certificates-by-themselves")
def test_a_second_domain_not_pointing_here_never_breaks_the_primarys_certificate(
        domainless_stack):
    """Outcome 57: with the primary's trusted certificate serving, the admin adds
    a second domain whose name does not point at the nest yet. An issuance cycle
    runs, leaves the second domain out, and the primary keeps its trusted
    certificate; once the second domain's record points here, the next cycle
    issues a certificate that covers it too (`tls-certificates.md` § HTTP-01 —
    a secondary apex joins the all-or-nothing order only once it resolves)."""
    nest, _ = domainless_stack
    host, port = "127.0.0.1", nest["port"]
    # The mail bring-up in the first test overrides the nest's own listener with a
    # self-signed certificate (`provision_self_signed_cert`, which keeps the real
    # one aside) — a harness step no real deploy takes. Undo it the way an admin
    # would ("switch back to a real certificate") before the premise is read.
    def _primary_trusted_after_restore():
        _issuance_now(nest)
        return _leaf_is_pebble(served_leaf(host, port, ADDED_DOMAIN))

    wait_until(
        _primary_trusted_after_restore, 120, interval=5,
        diagnose=lambda: (
            "the premise: the primary serves its trusted certificate before the "
            "second domain is added — still self-signed after restoring the real one; "
            f"issuer={served_leaf(host, port, ADDED_DOMAIN).issuer.rfc4514_string()!r}, "
            f"cert_status={_cert_status(nest, [ADDED_DOMAIN]).get(ADDED_DOMAIN)!r}\n"
            f"--- bridge_diag ---\n{bridge_diag(nest['name'])}"
        ),
    )

    # Add the second domain, unpublished, and run an issuance cycle now.
    deferred_before = _deferrals_of_second(nest["name"])
    with admin_ws(nest) as admin:
        admin.call("fauna.bridges.add_local_domain", {
            "domain": SECOND_DOMAIN,
            "mta_sts_cert_mode": "expand_primary",
        })
    _issuance_now(nest)
    wait_until(
        lambda: _deferrals_of_second(nest["name"]) > deferred_before, 120, interval=2,
        diagnose=lambda: (
            "no issuance cycle ran with the second domain added (no "
            f"{SECONDARY_DEFERRED_LOG!r} line for {SECOND_DOMAIN}) — the premise "
            "of the outcome, that a cycle saw the unpublished domain, never held\n"
            f"--- bridge_diag ---\n{bridge_diag(nest['name'])}"
        ),
    )

    # That cycle left the primary's certificate whole and trusted.
    leaf = served_leaf(host, port, ADDED_DOMAIN)
    status = _cert_status(nest, [ADDED_DOMAIN])[ADDED_DOMAIN]
    assert _leaf_is_pebble(leaf), (
        "adding a second domain that does not point here took the primary off its "
        f"trusted certificate: issuer={leaf.issuer.rfc4514_string()!r}"
    )
    assert status["state"] == "ValidTrusted" and status["is_floor"] is False, status
    assert {ADDED_DOMAIN, MX_SNI} <= set(_sans(leaf)), _sans(leaf)
    assert SECOND_DOMAIN not in _sans(leaf), (
        f"the unpublished second domain was put into the order: {_sans(leaf)!r}")

    # Point the second domain here: the next cycle covers it as well.
    _publish_dns(nest["records_dir"], nest["ip"], second=True)
    next_wake = [0.0]

    def _second_joined():
        # Wake the cycle at most every 20 s; read the served certificates every poll.
        if time.monotonic() >= next_wake[0]:
            _issuance_now(nest)
            next_wake[0] = time.monotonic() + 20
        primary = served_leaf(host, port, ADDED_DOMAIN)
        second = served_leaf(host, port, SECOND_DOMAIN)
        joined = (_leaf_is_pebble(primary) and SECOND_DOMAIN in _sans(primary)
                  and _leaf_is_pebble(second))
        return primary if joined else None

    leaf = wait_until(
        _second_joined, 180, interval=2,
        diagnose=lambda: (
            "the second domain, once pointing here, never joined the trusted "
            f"certificate: primary sans={_sans(served_leaf(host, port, ADDED_DOMAIN))!r}, "
            "second-domain issuer="
            f"{served_leaf(host, port, SECOND_DOMAIN).issuer.rfc4514_string()!r}\n"
            f"--- bridge_diag ---\n{bridge_diag(nest['name'])}"
        ),
    )
    assert {ADDED_DOMAIN, MX_SNI} <= set(_sans(leaf)), (
        f"the re-issued certificate dropped a primary name: {_sans(leaf)!r}")


def _service_pid(name: str, service: str) -> int | None:
    """The s6-supervised process id of `service` (`up (pid N pgid N) …`), or None."""
    m = re.match(r"up \(pid (\d+)", svstat(name, service))
    return int(m.group(1)) if m else None


@pytest.mark.feature("certificates-by-themselves")
def test_a_self_signed_stopgap_heals_to_trusted_and_reaches_mail_and_calendar_without_restart(
        domainless_stack):
    """Outcomes 6 and 12, on the box the tests above left (trusted cert issued,
    mail serving). The admin puts a self-signed certificate in place over the
    trusted one — the stopgap `provision_self_signed_cert` writes. With nobody
    acting after that, the nest replaces it with a NEW trusted certificate
    (`tls-certificates.md` § Keeping the cert alive — the self-heal), and that
    newly issued certificate is what the mail port (IMAPS 993) and the calendar
    port (CalDAV, behind the SNI router's 443) serve within moments — with the
    mail bridge's process never restarted (the `"tls"` config-changed push makes
    it re-fetch; the bring-up's restarts and SIGHUPs are all behind us)."""
    nest, mail_ports = domainless_stack
    host, port = "127.0.0.1", nest["port"]
    mda = "fauna-mail-bridge-mda"

    def _trusted():
        leaf = served_leaf(host, port, ADDED_DOMAIN)
        return leaf if _leaf_is_pebble(leaf) else None

    before = wait_until(
        _trusted, 120, interval=2,
        diagnose=lambda: (
            "the premise: the box serves a trusted cert before the stopgap; issuer="
            f"{served_leaf(host, port, ADDED_DOMAIN).issuer.rfc4514_string()!r}"),
    )
    mda_pid = _service_pid(nest["name"], mda)
    assert mda_pid, f"the premise: the MDA is up; svstat={svstat(nest['name'], mda)!r}"

    reply = provision_self_signed_cert(nest, ADDED_DOMAIN)
    assert reply.get("expires_at_unix"), reply

    def _healed():
        leaf = served_leaf(host, port, ADDED_DOMAIN)
        fresh = _leaf_is_pebble(leaf) and leaf.serial_number != before.serial_number
        return leaf if fresh else None

    healed = wait_until(
        _healed, 180, interval=2,
        diagnose=lambda: (
            "the self-signed stopgap was never replaced by a new trusted cert: issuer="
            f"{served_leaf(host, port, ADDED_DOMAIN).issuer.rfc4514_string()!r}, "
            f"cert_status={_cert_status(nest, [ADDED_DOMAIN]).get(ADDED_DOMAIN)!r}\n"
            f"--- bridge_diag ---\n{bridge_diag(nest['name'])}"),
    )
    assert MX_SNI in _sans(healed), _sans(healed)

    # The newly issued cert reaches the mail and calendar ports, no restart.
    def _on_mail_and_calendar():
        imaps = served_leaf(host, mail_ports[993], MX_SNI)
        caldav = served_leaf(host, nest["router_port"], MX_SNI)
        return (imaps.serial_number == healed.serial_number
                and caldav.serial_number == healed.serial_number)

    def _diagnose_ports():
        imaps = served_leaf(host, mail_ports[993], MX_SNI)
        caldav = served_leaf(host, nest["router_port"], MX_SNI)
        return (
            f"the new cert (serial {healed.serial_number:x}) did not reach the mail and "
            f"calendar ports: IMAPS serial {imaps.serial_number:x} issuer "
            f"{imaps.issuer.rfc4514_string()!r}, CalDAV serial {caldav.serial_number:x} "
            f"issuer {caldav.issuer.rfc4514_string()!r}\n"
            f"--- bridge_diag ---\n{bridge_diag(nest['name'])}")

    wait_until(_on_mail_and_calendar, 90, interval=2, diagnose=_diagnose_ports)
    assert _service_pid(nest["name"], mda) == mda_pid, (
        "the mail bridge was restarted to pick up the new cert; it must be served "
        f"without one (pid {mda_pid} → svstat {svstat(nest['name'], mda)!r})")

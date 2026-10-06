"""tier_4 e2e: the forwarder non-delivery report (NDR) seals locally — no MX
hairpin — in the real Docker deploy image.

Regression guard for the **containerized SRS0 hairpin** class
(`docs/goal/behavior/mail-forwarding.md` § NDR routing / N4b;
tracked internally). When OUR outbound
worker permanently fails to deliver a *forwarded* message, the synchronous-permfail
DSN must be sealed STRAIGHT into the forwarder's sealed INBOX
(`generate_forwarder_ndr` → `seal_and_ingest_local`), never enqueued back out over
the MX. The OLD design enqueued the NDR to a ``SRS0=…@<primary-domain>`` envelope
and relied on inbound SRS-decode loopback; on a containerized deploy that
hairpinned through the docker bridge gateway (``172.18.0.1``, NOT loopback, so the
always-on sender-domain check does not exempt it) and ``554``-bounced at the
inbound HELO-identity check — the same self-loop class fixed for in-domain mailbox
/ DSN / auto-reply / security delivery (the ``submit_outbound`` partition).

The seal-and-deliver mechanism is already covered at lower tiers — the
**process-level** tier_3 ``test_forwarded_permfail_seals_dsn_to_forwarder_inbox_no_relay``
(real binaries, the per-user ``forward_all_to`` shape) and the Rust unit
``forwarder_dsn_body_carries_rule_and_recipients``. This tier_4 adds the one delta
those cannot reach: does the permfail→DSN path specifically AVOID hairpinning
through the docker bridge gateway on the **real image** + s6-supervised sidecars,
exercised over the **admin external-forwarder** shape (a bare ``kind='forwarder'``
alias with no underlying mailbox — distinct from the tier_3 ``forward_all_to``
path, and the shape that keeps NO local copy → the admin INBOX holds exactly the
one DSN, read cleanly via ``imap_fetch_only_inbox_message``).

What it asserts, end-to-end through every packaged binary (Go MTA ↔ nest ↔ MDA):
  1. Bring the deploy image's bridges to *serving*.
  2. An admin ``create_forwarder`` maps ``forwardme@<domain>`` → a downstream the
     stub external MX 550-rejects at RCPT (local part starts ``nonexistent``).
  3. One inbound message to ``forwardme@<domain>`` (MAIL FROM a domain mapped to
     the stub MX) clears the fail-closed scan gate and resolves to a Forward; the
     Go MTA dispatches the forward, the outbound worker relays it to the stub, the
     stub 550s → ``mark_outbound_bounced`` → nest's ``generate_forwarder_ndr``
     seals the DSN into the **admin** forwarder's INBOX.
  4. **Sealed, not relayed:** the DSN is read back DECRYPTED over IMAPS(993) from
     the admin's INBOX (multipart/report, ``From: postmaster@<domain>``,
     ``Final-Recipient`` = the downstream target, the forward-rule blurb), AND the
     stub external MX captures NO ``multipart/report`` — the regression guard.

**No-hairpin tightness.** The inbound MAIL FROM uses ``<probe>@<EXTERNAL_DOMAIN>``,
the one domain the ``mta_mx_override`` routes to the stub. So a *regressed* relayed
NDR — addressed to the original sender — would land at the stub, where this test
would catch it. With the fix it seals locally, so the stub stays empty. (The
forward's own relay is 550-rejected at RCPT before DATA, so it leaves no ``.eml``.)

**Prod-parity posture (Gap 2g — ``testing.md`` § Gap 2 Target).** The box runs the
live-box shape, not the relaxed opt-out: storage mode **Encrypted** (example.com's
default) and the inbound AUTH perimeter **ENFORCED** (``enforce_dmarc=true``/
``log_only=false``) via the shared ``enforce_mail_perimeter`` +
``publish_passing_mail_dns`` helpers. ``EXTERNAL_DOMAIN`` publishes a passing SPF
(``ip4:127.0.0.1`` — the loopback client IP) + ``_dmarc … p=reject`` through a
fake_dns sidecar (the nest's ``--dns``), so the enforced inbound DMARC gate accepts
the ``@external.test`` MAIL FROM only by an aligned auth pass — the
forward→permfail→NDR path now runs the same perimeter the live box does. (The
sender-domain A-check is itself loopback-exempt, so this replaces the old
``/etc/hosts`` entry the stale "no loopback exemption" belief added.) The fake_dns
self-MX for ``EXTERNAL_DOMAIN`` is unused: the ``mta_mx_override`` wins for the
forward's relay target. The NDR seal/store/IMAP-decrypt path is
storage-mode-independent (msek-gated, not ``StorageMode``-gated), so
Encrypted exercises identical mail code.
"""

import os
import shutil
import tempfile
import time

import pytest


from .helpers import (
    IMAGE_TAG,
    admin_ws,
    bridge_diag,
    bring_bridges_to_serving,
    claim_admin_api,
    create_network,
    deliver_inbound_loopback_curl,
    docker_build,
    enforce_mail_perimeter,
    find_free_port,
    find_free_ports,
    get_repo_root,
    imap_fetch_only_inbox_message,
    provision_admin_mailbox,
    publish_passing_mail_dns,
    read_stub_mx_message,
    remove_container,
    remove_network,
    start_container_with_ports,
    start_fake_dns_sidecar,
    start_fake_scanner_sidecars,
    start_stub_mx_sidecar,
    wait_for_health,
)

try:
    import subprocess

    subprocess.run(["docker", "info"], capture_output=True, timeout=10)
    HAS_DOCKER = True
except Exception:
    HAS_DOCKER = False

pytestmark = [
    pytest.mark.skipif(not HAS_DOCKER, reason="Docker not available"),
    pytest.mark.tier_4,
]

DOMAIN = "localhost"               # the deployment's primary (local) mail domain
EXTERNAL_DOMAIN = "external.test"  # routed to the stub MX (outbound) + /etc/hosts (inbound)
CLAIM_CODE = "FWDNDR"
ADMIN_MAILBOX_LOCAL = "fwdadmin"   # the admin's IMAP mailbox (distinct from its "admin" handle)
ADMIN_MAILBOX_PASSWORD = "forwarder-ndr-password-1"
FORWARD_PATTERN = "forwardme"      # the inbound address that resolves to the admin forwarder


# ── Fixtures ──────────────────────────────────────────────────────────


@pytest.fixture(scope="module")
def docker_image():
    """Build the image once for this module (shared tag, layer-cached)."""
    docker_build(get_repo_root())
    yield IMAGE_TAG


@pytest.fixture()
def ndr_sidecars(docker_image, tmp_path):
    """All sidecars on ONE user-defined network the nest joins: fake clamd + rspamd
    (the fail-closed inbound scan gate the forwarded message must clear), a stub
    external MX (the forward's relay target — and where a regressed relayed NDR
    would land), and a ``records_dir``-backed fake_dns resolver (the nest's
    ``--dns``) so the inbound sender domain can publish a PASSING SPF + ``_dmarc …
    p=reject`` for the ENFORCED DMARC perimeter (Gap 2g prod parity — ``testing.md``
    § Gap 2 Target). Yields
    {network, clamd_addr, rspamd_url, mx_target, out_dir, dns_ip, records_dir}."""
    suffix = find_free_port()  # unique-enough token for the network + container names
    network = f"fauna-ndr-net-{suffix}"
    clamd_name = f"fauna-ndr-clamd-{suffix}"
    rspamd_name = f"fauna-ndr-rspamd-{suffix}"
    stub_name = f"fauna-ndr-stubmx-{suffix}"
    dns_name = f"fauna-ndr-dns-{suffix}"
    fakes_dir = str(get_repo_root() / "tests" / "e2e-unified" / "fakes")
    helpers_dir = str(get_repo_root() / "tests" / "e2e-unified" / "helpers")
    out_dir = tempfile.mkdtemp(prefix="fauna-ndr-stubmx-")
    # The stub MX (container root, possibly userns-remapped) writes deliveries
    # here through a bind mount; world-writable so the write succeeds regardless
    # of the daemon's uid-remap policy.
    os.chmod(out_dir, 0o777)
    records_dir = str(tmp_path)
    os.chmod(records_dir, 0o777)  # the fake_dns sidecar reads it through a bind mount
    create_network(network)
    try:
        scan = start_fake_scanner_sidecars(
            network, fakes_dir, clamd_name=clamd_name, rspamd_name=rspamd_name)
        mx_target = start_stub_mx_sidecar(network, helpers_dir, out_dir, name=stub_name)
        dns_ip = start_fake_dns_sidecar(
            network, fakes_dir, name=dns_name, txt_records={}, records_dir=records_dir)
        yield {"network": network, "mx_target": mx_target, "out_dir": out_dir,
               "dns_ip": dns_ip, "records_dir": records_dir, **scan}
    finally:
        remove_container(clamd_name)
        remove_container(rspamd_name)
        remove_container(stub_name)
        remove_container(dns_name)
        remove_network(network)
        shutil.rmtree(out_dir, ignore_errors=True)


@pytest.fixture()
def forwarder_ndr_nest(docker_image, ndr_sidecars):
    """Fresh claimed container on the sidecars' network with all four mail ports
    mapped, the scan gate pointed at the fake scanners, ``external.test`` routed to
    the stub MX (outbound), and the resolver (``--dns``) pointed at the fake_dns
    sidecar so the inbound ``@external.test`` MAIL FROM resolves a PASSING SPF +
    ``_dmarc`` for the enforced perimeter (Gap 2g — ``testing.md`` § Gap 2 Target).
    Storage mode committed **Encrypted** — example.com's privacy-respecting default;
    the forwarder NDR is sealed into the admin INBOX and read back decrypted, and
    that seal/store/IMAP-decrypt path is storage-mode-independent (msek-gated, not
    ``StorageMode``-gated). Cleaned up after."""
    http_port, *mail = find_free_ports(5)
    mail_ports = dict(zip((25, 465, 587, 993), mail))  # container_port -> host_port
    name = f"fauna-mail-fwdndr-{http_port}"
    start_container_with_ports(
        name,
        {3000: http_port, **mail_ports},
        env={
            "FAUNA_CLAIM_CODE": CLAIM_CODE,
            "FAUNA_PORT": "3000",
            # Inbound: the fail-closed scan gate dials the sidecar scanners by name.
            "FAUNA_CLAMD_ADDR": ndr_sidecars["clamd_addr"],
            "FAUNA_RSPAMD_URL": ndr_sidecars["rspamd_url"],
            # Outbound: relay external.test to the stub-MX sidecar by name. This is
            # an explicit next-hop override (`OverrideMXResolver`) that takes priority
            # over DNS — so it wins over the self-MX the fake_dns publishes for
            # external.test, which exists only for the inbound sender's SPF/_dmarc.
            "FAUNA_MTA_MX_OVERRIDE": f"{EXTERNAL_DOMAIN}={ndr_sidecars['mx_target']}",
        },
        # The resolver points at the fake_dns sidecar so `verify_inbound` reads the
        # @external.test sender's published passing SPF/_dmarc for the ENFORCED DMARC
        # gate (Gap 2g). Container-name resolution (scanners, stub MX) still goes via
        # docker's embedded DNS; only external names forward to fake_dns. The loopback
        # delivery is exempt from the reachability-gated HELO/FCrDNS checks.
        dns=ndr_sidecars["dns_ip"],
        network=ndr_sidecars["network"],
    )
    try:
        wait_for_health(http_port, name)
        admin = claim_admin_api(http_port, CLAIM_CODE, handle="admin")
        yield {
            "name": name,
            "port": http_port,
            "url": f"https://127.0.0.1:{http_port}",
            "admin": admin,
            "mail_ports": mail_ports,
            "out_dir": ndr_sidecars["out_dir"],
            "records_dir": ndr_sidecars["records_dir"],
        }
    finally:
        remove_container(name)


# ── Test ──────────────────────────────────────────────────────────────


@pytest.mark.feature("mail-server", "admin-forwarders")
def test_mail_deploy_forwarder_ndr_seals_locally_no_hairpin(forwarder_ndr_nest, run_seal_helper):
    nest = forwarder_ndr_nest
    name = nest["name"]
    mp = nest["mail_ports"]  # container_port -> host_port
    out_dir = nest["out_dir"]
    records_dir = nest["records_dir"]

    token = f"fwdndr-{int(time.time() * 1000)}"
    # Local part starts `nonexistent` → the stub MX 550s the RCPT (deterministic
    # permanent 5xx → immediate bounce, no retry wait).
    forward_target = f"nonexistent-bounce-{token}@{EXTERNAL_DOMAIN}"
    admin_addr = f"{ADMIN_MAILBOX_LOCAL}@{DOMAIN}"
    forwarder_addr = f"{FORWARD_PATTERN}@{DOMAIN}"
    # MAIL FROM a domain the override routes to the stub, so a regressed relayed
    # NDR (addressed to the original sender) would be observable at the stub.
    sender = f"probe-{token}@{EXTERNAL_DOMAIN}"

    # nest-side setup before bring-up, via the shared Gap-2g prod-parity helper:
    # register the primary domain (serving ordering), relax ONLY the orthogonal
    # spam gates a synthetic loopback peer can't satisfy, and ENFORCE the DMARC
    # perimeter (`enforce_dmarc=true`, `log_only=false`). Then provision the admin a
    # mailbox (the forwarder NDR seals to the admin actor) and create the admin
    # external forwarder.
    enforce_mail_perimeter(nest, domain=DOMAIN)
    provision_admin_mailbox(
        nest, run_seal_helper, domain=DOMAIN, local_part=ADMIN_MAILBOX_LOCAL,
        password=ADMIN_MAILBOX_PASSWORD,
    )
    with admin_ws(nest) as admin:
        admin.call("fauna.bridges.create_forwarder",
                   {"local_domain": DOMAIN, "pattern": FORWARD_PATTERN,
                    "forward_target": forward_target})

    # Publish the inbound sender domain's PASSING perimeter into the fake_dns: a
    # passing SPF authorizing the loopback (`spf_ip=127.0.0.1` — the loopback delivery
    # connects from inside the container, so the MTA sees 127.0.0.1 as the client IP)
    # + `_dmarc … p=reject`. Under the enforced gate the @external.test inbound is
    # accepted ONLY because it DMARC-passes via the aligned SPF pass — so the
    # forward→permfail→NDR path now runs the full prod-parity perimeter. (The self-MX
    # this also publishes for external.test is unused: the FAUNA_MTA_MX_OVERRIDE wins
    # for the outbound forward's next hop.)
    publish_passing_mail_dns(records_dir, {EXTERNAL_DOMAIN: "127.0.0.1"}, spf_ip="127.0.0.1")

    bring_bridges_to_serving(name, nest, mp, DOMAIN)

    # (forward → permfail) One inbound message to the forwarder address, from
    # inside the container over loopback (the DNS-dependent HELO/FCrDNS perimeter is
    # loopback-exempt; the sender-domain A-check is loopback-exempt too, and
    # external.test's SPF/_dmarc resolve via fake_dns for the enforced DMARC gate).
    # The MTA resolves the recipient to a Forward, dispatches it,
    # and the outbound worker relays it to the stub — which 550s the RCPT → the
    # bridge classifies permanent → nest seals the forwarder NDR into the admin's
    # INBOX (no MX relay).
    subject = f"forwarder-ndr probe {token}"
    deliver_inbound_loopback_curl(
        name,
        mail_from=sender,
        rcpt_to=forwarder_addr,
        subject=subject,
        body_text=f"Forward me to a dead downstream so our worker gives up ({token}).\n",
    )

    # (sealed, not relayed — read back decrypted) IMAPS AUTH PLAIN → SELECT INBOX
    # → UID FETCH BODY[]. The admin forwarder keeps NO local copy, so its INBOX
    # holds exactly the one sealed DSN; the MDA decrypts it server-side.
    try:
        raw = imap_fetch_only_inbox_message(
            "127.0.0.1", mp[993], DOMAIN, admin_addr, ADMIN_MAILBOX_PASSWORD,
        )
    except (AssertionError, TimeoutError) as e:
        raise AssertionError(
            f"the forwarder NDR was not sealed into the admin forwarder's INBOX: {e}\n\n"
            f"── bridge diagnostics ──\n{bridge_diag(name)}") from e

    text = raw.decode("utf-8", errors="replace")
    assert "multipart/report" in text, (
        f"the sealed bounce must be an RFC 3464 multipart/report DSN; "
        f"got first 600B: {text[:600]!r}")
    assert f"postmaster@{DOMAIN}" in text, (
        f"the DSN must be From: postmaster@{DOMAIN}; got first 600B: {text[:600]!r}")
    assert f"Final-Recipient: rfc822; {forward_target}" in text, (
        f"the DSN must name the downstream target as the Final-Recipient; "
        f"got first 600B: {text[:600]!r}")
    assert "This bounce was generated because your forward-rule" in text, (
        f"the DSN must carry the forward-rule blurb; got first 600B: {text[:600]!r}")
    assert token in text, (
        "the DSN must embed the original message (carrying the run token) so the "
        f"forwarder can tell which message bounced; got first 600B: {text[:600]!r}")

    # (no hairpin — the regression guard) The DSN seals locally, so nothing tied
    # to this run reaches the downstream MX. A regressed relayed NDR would have
    # been addressed to the original sender (probe-<token>@external.test → the stub
    # via the override) and captured here; the forward's own relay is 550-rejected
    # at RCPT before DATA, so it leaves no .eml. The DSN already landed above, so
    # any erroneous relay would already be in flight; a short grace covers latency.
    leaked = read_stub_mx_message(out_dir, token, timeout=15.0)
    assert leaked is None, (
        "regression: a forwarder DSN was relayed out to the downstream MX (the "
        "containerized SRS0 hairpin is back) — it must be sealed locally instead. "
        f"Captured at the stub:\n{(leaked or b'')[:600]!r}\n\n"
        f"── bridge diagnostics ──\n{bridge_diag(name)}")

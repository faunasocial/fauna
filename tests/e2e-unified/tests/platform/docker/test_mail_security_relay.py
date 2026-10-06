"""tier_4 e2e: the MTA security perimeter in the real Docker deploy image.

Track 3 of the mail-deployment VPS milestone
(tracked internally; SECURITY check): the deployed
mail server must NOT be exploitable as an open relay, its submission ports must
NOT accept unauthenticated relay, and it must NOT accept unauthenticated mail
that spoofs one of its own (local) domains. The first two are
*reject-before-delivery* properties (they refuse the envelope outright, no DATA);
the third (anti-spoofing) is a *reject-at-DATA* property — the message clears the
envelope, but ``verify_inbound`` computes a DMARC ``Fail`` against the spoofed
domain's published ``p=reject`` policy and the auth-enforce stage rejects it
before the message is ever stored.

Why tier_4 and not the existing Go unit coverage: the relay gate
(``server_test.go`` proves ``550 5.7.1`` at the ``Rcpt`` handler) and the
submission-auth gate (``submission_test.go`` proves ``530`` at ``Mail``) are
unit-tested in isolation, but those tests bypass the *deployed image* — the s6
supervision, the cold-boot config-fetch that populates ``local_domains``, the
real TLS listeners on 25/465/587. An open relay that ships only because the
packaged image wires the listener wrong is exactly the class tier_4 exists to
catch. The properties are the most security-critical
ones in the mail stack — an open relay is an internet-abuse incident.

What it asserts, against the real image with the bridges brought to *serving*:
  1. **No open relay (port 25).** An unauthenticated inbound RCPT TO an
     **external** (non-``local_domains``) recipient is rejected ``550 5.7.1``.
     Delivered from inside the container over loopback, which clears the
     DNS-dependent HELO/FCrDNS/sender-domain perimeter — so the rejection
     observed is unambiguously the *relay* gate (``server.go::Rcpt``), which has
     **no** loopback exemption. The spam policy is fully relaxed, proving relay
     denial is independent of the spam perimeter (it is unconditional).
  2. **No unauthenticated submission relay (587 + 465).** An unauthenticated
     ``MAIL FROM`` on either submission port is rejected ``530 5.7.0`` — the
     listeners are ``AllowInsecureAuth = false`` and gate ``MAIL FROM`` on a
     prior AUTH (``submission.go::Mail``). There is no unauthenticated send path.
  3. **No local-domain spoofing (DMARC ``p=reject``).** An unauthenticated
     inbound message whose ``From:`` is one of the deployment's own local domains,
     arriving from an unauthorized source, is rejected ``550 5.7.1 DMARC reject``.
     The verdict needs DNS-published policy (``verify_inbound`` reads the
     container's ``/etc/resolv.conf`` — ``new_system_conf()``, no env override),
     so a ``fakes/fake_dns`` sidecar publishes ``_dmarc.<domain> p=reject`` plus a
     hard-fail SPF, wired in via ``docker run --dns``. The message is delivered
     over loopback to a *valid* local recipient (so it clears the relay gate and
     reaches DATA), but ``mta/auth_enforce.go::applyDMARCRejectGate`` rejects it
     at DATA before the (later) scan gate — proving the deployed image enforces
     the anti-spoofing perimeter, not just the Go unit coverage.

Spec: ``docs/goal/behavior/smtp-server.md`` § Error / tempfail strategy (the
``Relay denied`` + ``DMARC reject`` rows), § Auth on each port
(``AllowInsecureAuth = false`` on 465/587), and § Deploy-image security perimeter
(the previously "not yet guarded at tier_4" anti-spoofing property).
"""

import subprocess

import pytest


from .helpers import (
    IMAGE_TAG,
    add_local_domain,
    bridge_diag,
    bring_bridges_to_serving,
    claim_admin_api,
    create_network,
    deliver_inbound_attempt_loopback_curl,
    docker_build,
    find_free_port,
    find_free_ports,
    get_repo_root,
    provision_mail_recipient,
    put_auth_policy,
    register_primary_domain,
    relax_spam_policy,
    relay_attempt_loopback_curl,
    remove_container,
    remove_network,
    start_container_with_ports,
    start_fake_dns_sidecar,
    unauth_submission_mail_code,
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
]

DOMAIN = "localhost"
CLAIM_CODE = "SECRLY"

# Anti-spoofing test: a second, real-looking local domain whose DMARC policy the
# fake-DNS sidecar publishes as `p=reject`. `localhost` stays the *primary* mail
# domain (the self-signed bootstrap cert covers it → the listener serves TLS);
# this is a non-primary local domain (`is_primary` is nest-derived, first-active),
# so it routes/validates recipients without needing its own serving cert. `.test`
# is a reserved TLD (RFC 6761) the public-suffix list treats as a normal TLD, so
# `victim.test` is the registrable domain and `_dmarc.victim.test` is the exact
# DMARC record location (no org-domain fallback needed).
SPOOF_DOMAIN = "victim.test"
SPOOF_CLAIM_CODE = "SPOOF1"
SPOOF_RECIPIENT_LOCAL = "ceo"
SPOOF_RECIPIENT_PASSWORD = "anti-spoof-password-1"


# ── Fixtures ──────────────────────────────────────────────────────────


@pytest.fixture(scope="module")
def docker_image():
    """Build the image once for this module (shared tag, layer-cached)."""
    docker_build(get_repo_root())
    yield IMAGE_TAG


@pytest.fixture(scope="module")
def serving_nest(docker_image):
    """A fresh claimed container with the four mail ports mapped and the bridges
    brought to *serving* — shared across both reject tests (each is a read-only
    rejection, no state mutation, so one boot suffices).

    No scanner sidecars / user-defined network: both tests reject *before* DATA
    (relay at RCPT, submission-auth at MAIL FROM), so the DATA-stage scan gate is
    never reached. ``host.docker.internal`` need only *resolve* (the inbound
    sender-domain check on the loopback relay attempt). Storage committed
    plaintext (the deploy default) so the self-signed cert provisions.
    """
    http_port, *mail = find_free_ports(5)
    mail_ports = dict(zip((25, 465, 587, 993), mail))  # container_port -> host_port
    name = f"fauna-mail-security-{http_port}"
    start_container_with_ports(
        name,
        {3000: http_port, **mail_ports},
        env={
            "FAUNA_CLAIM_CODE": CLAIM_CODE,
            "FAUNA_PORT": "3000",
        },
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
            "mail_ports": mail_ports,
        }
        register_primary_domain(nest, DOMAIN)
        # Fully relax the spam perimeter: this PROVES relay denial is independent
        # of it — even with dnsbl/greylist/fcrdns off, RCPT to an external domain
        # is still rejected. (Also lets the loopback peer past the connect-time
        # DNSBL that would otherwise 550 a 127.0.0.1 reverse before RCPT.)
        relax_spam_policy(nest)
        try:
            bring_bridges_to_serving(name, nest, mail_ports, DOMAIN)
        except (AssertionError, TimeoutError) as e:
            raise AssertionError(
                f"{e}\n\n── bridge diagnostics ──\n{bridge_diag(name)}") from e
        yield nest
    finally:
        remove_container(name)


# ── Tests ──────────────────────────────────────────────────────────────


@pytest.mark.feature("mail-abuse-refused")
def test_no_open_relay_on_port25(serving_nest):
    """Unauthenticated port-25 RCPT TO an external recipient → 550 5.7.1."""
    name = serving_nest["name"]
    rc, output = relay_attempt_loopback_curl(
        name,
        mail_from="sender@host.docker.internal",
        rcpt_to="victim@external-not-served.example",
    )
    diag = lambda: f"\n\n── bridge diagnostics ──\n{bridge_diag(name)}"  # noqa: E731
    assert rc != 0, (
        "OPEN RELAY: the bridge accepted an external recipient on port 25 "
        f"(curl exited 0). Output: {output!r}{diag()}"
    )
    # curl surfaces only the primary SMTP code on a RCPT rejection ("RCPT
    # failed: 550"), not the enhanced code / message, so we assert on `550`. A
    # 550 on an **external** RCPT is unambiguously the relay gate: it is the
    # FIRST check in `server.go::inboundSession.Rcpt` (foreign-domain →
    # `550 5.7.1 Relay access denied`), ahead of recipient validation (which
    # only runs for a local domain). So a 550 here can only mean relay-denied.
    assert "550" in output, (
        "external RCPT must be rejected as relay (550 5.7.1 Relay access "
        f"denied); got curl output: {output!r}{diag()}"
    )


@pytest.mark.parametrize(
    "port_key,implicit_tls",
    [(587, False), (465, True)],
    ids=["starttls-587", "implicit-tls-465"],
)
@pytest.mark.feature("mail-abuse-refused")
def test_submission_requires_auth(serving_nest, port_key, implicit_tls):
    """Unauthenticated MAIL FROM on a submission port → 530 5.7.0."""
    name = serving_nest["name"]
    host_port = serving_nest["mail_ports"][port_key]
    code, msg = unauth_submission_mail_code(
        "127.0.0.1", host_port, implicit_tls=implicit_tls,
        mail_from="attacker@host.docker.internal",
    )
    text = msg.decode(errors="replace") if isinstance(msg, bytes) else str(msg)
    assert code == 530, (
        f"UNAUTH RELAY: submission port {port_key} accepted MAIL FROM without "
        f"AUTH (got {code} {text!r}, want 530 5.7.0 Authentication required)"
        f"\n\n── bridge diagnostics ──\n{bridge_diag(name)}"
    )


# ── Anti-spoofing (DMARC p=reject) ───────────────────────────────────────


@pytest.fixture()
def spoofing_nest(docker_image):
    """A fresh claimed container whose resolver points at a ``fake_dns`` sidecar
    publishing ``_dmarc.victim.test p=reject`` + a hard-fail SPF for the same
    domain. ``localhost`` is the primary mail domain (TLS-cert path); ``victim.test``
    is added as a non-primary local domain in the test body (the spoof target).

    Topology: a user-defined network carrying the DNS sidecar, with the nest
    joined to it and ``--dns <sidecar-ip>`` so ``verify_inbound``'s resolver reads
    the published policy. No scanner sidecars: the DMARC reject fires at C.5
    auth-enforce, *before* the C.7 fail-closed scan gate, so the scanners are
    never reached. Storage committed plaintext (deploy default → self-signed cert
    provisions). Cleaned up after."""
    suffix = find_free_port()  # unique token for network + sidecar names
    network = f"fauna-spoof-net-{suffix}"
    dns_name = f"fauna-spoof-dns-{suffix}"
    fakes_dir = str(get_repo_root() / "tests" / "e2e-unified" / "fakes")
    http_port, *mail = find_free_ports(5)
    mail_ports = dict(zip((25, 465, 587, 993), mail))  # container_port -> host_port
    name = f"fauna-mail-spoof-{http_port}"
    create_network(network)
    try:
        dns_ip = start_fake_dns_sidecar(
            network, fakes_dir, name=dns_name,
            txt_records={
                f"_dmarc.{SPOOF_DOMAIN}": "v=DMARC1; p=reject",
                # A hard SPF fail (`-all`, no authorized hosts) for the spoofed
                # sender — guarantees SPF cannot align/pass, so DMARC has no
                # passing authenticated identifier and falls to `p=reject`.
                SPOOF_DOMAIN: "v=spf1 -all",
            },
        )
        start_container_with_ports(
            name,
            {3000: http_port, **mail_ports},
            env={
                "FAUNA_CLAIM_CODE": SPOOF_CLAIM_CODE,
                "FAUNA_PORT": "3000",
            },
            network=network,
            dns=dns_ip,
        )
        try:
            wait_for_health(http_port, name)
            admin = claim_admin_api(http_port, SPOOF_CLAIM_CODE, handle="admin")
            nest = {
                "name": name,
                "port": http_port,
                "url": f"https://127.0.0.1:{http_port}",
                "admin": admin,
                "mail_ports": mail_ports,
            }
            yield nest
        finally:
            remove_container(name)
    finally:
        remove_container(dns_name)
        remove_network(network)


@pytest.mark.feature("mail-abuse-refused")
def test_local_domain_spoofing_rejected(spoofing_nest, run_seal_helper):
    """Unauthenticated inbound spoofing a local domain (DMARC ``p=reject``) →
    rejected ``550 5.7.1`` at DATA."""
    nest = spoofing_nest
    name = nest["name"]

    # Setup targets nest (not the bridge), so before enable: localhost is the
    # primary (cert) domain, victim.test is the spoofed local domain, the spam
    # perimeter is relaxed (loopback peer accepted), DMARC enforcement is ON, and
    # a valid recipient lives on victim.test so RCPT passes and the message
    # reaches DATA (where DMARC rejects it).
    register_primary_domain(nest, DOMAIN)
    add_local_domain(nest, SPOOF_DOMAIN)
    relax_spam_policy(nest)
    put_auth_policy(nest, enforce_dmarc=True, log_only=False)
    recipient = provision_mail_recipient(
        nest, run_seal_helper, domain=SPOOF_DOMAIN,
        local_part=SPOOF_RECIPIENT_LOCAL, password=SPOOF_RECIPIENT_PASSWORD,
    )

    bring_bridges_to_serving(name, nest, nest["mail_ports"], DOMAIN)

    # Deliver from inside the container over loopback (HELO/FCrDNS/sender-domain
    # checks are loopback-exempt → the message clears the envelope perimeter and
    # reaches DATA). The `From:` AND envelope sender both spoof victim.test — a
    # *local* domain — from the unauthorized loopback source: SPF `-all` fails,
    # there is no DKIM signature, so DMARC fails with the published `p=reject`.
    rc, output = deliver_inbound_attempt_loopback_curl(
        name,
        mail_from=f"spoofed-boss@{SPOOF_DOMAIN}",
        rcpt_to=recipient["username"],
        subject="anti-spoofing guard",
        body_text="This message spoofs a local domain and must be rejected.\n",
    )
    diag = lambda: f"\n\n── bridge diagnostics ──\n{bridge_diag(name)}"  # noqa: E731
    assert rc != 0, (
        "SPOOF ACCEPTED: the bridge accepted an unauthenticated message spoofing "
        f"the local domain {SPOOF_DOMAIN} (curl exited 0). DMARC p=reject must "
        f"reject it. Output: {output!r}{diag()}"
    )
    # The verbose curl trace carries the DATA-stage reply line. Assert the
    # SPECIFIC anti-spoofing verdict — `550 5.7.1 DMARC reject` from
    # `applyDMARCRejectGate`, the FIRST auth-enforce gate (ahead of SPF 5.7.23 /
    # DKIM 5.7.20). It is unambiguously DMARC, not relay-denied (also 5.7.1): the
    # recipient is a *valid local* address whose RCPT already returned 250, so the
    # rejection is at DATA, not at RCPT. This is the regression guard for the
    # verdict-mapping fix (`classify_dmarc`): an unauthenticated message with NO
    # SPF/DKIM pass under `p=reject` must fail DMARC — before the fix it slipped to
    # the SPF gate (550 5.7.23) and would have been ACCEPTED outright had the
    # spoofed domain not also published `-all`.
    assert "5.7.1" in output and "DMARC reject" in output, (
        "spoofing a local domain (DMARC p=reject) must be rejected with "
        f"`550 5.7.1 DMARC reject`; got curl output: {output!r}{diag()}"
    )

"""tier_4 e2e: the full inbound mail round-trip in the real Docker deploy image.

Stage 5 **Slice 2** of the mail-deployment VPS milestone
(tracked internally). Builds on the
serving proof in ``test_mail_deploy_lifecycle.py`` and adds the two halves the
milestone ultimately needs, chained end-to-end through the packaged binaries: an
external SMTP message accepted on port 25, HPKE-sealed to the user, then read
back **decrypted** over IMAPS(993). This is the ``green-test-or-it-doesn't-work``
deliverable — a 250-on-DATA proves *ingest*, not that the mail is *readable*.

Each half is individually proven at the process level
(``test_mail_bridge_mta.py::test_inbound_mx_round_trip`` stops at the ingest ACK;
``test_mail_bridge_mda.py`` reads an APPEND'd body back decrypted); this test
glues SMTP-inbound → store → IMAP-read in the **real image** with a **real
user**, the seam neither half exercises (plan § Parity-verification results).

What it asserts:
  1. Bring the deploy image's bridges to *serving* (the lifecycle sequence, via
     ``bring_bridges_to_serving``).
  2. **Receive:** an unauthenticated STARTTLS message to ``<user>@<domain>`` on
     port 25 clears the (default-on, *fail-closed*) clamd/rspamd scan gate and is
     accepted. The user is provisioned exactly as a primary client would — an
     MSEK-derived recipient MLS pubkey (the MTA seals the body to it), a
     wrapped-MSEK AUTH credential, and an MLS snapshot carrying the matching leaf
     secret — all keyed off **one** MSEK so the seal/open halves match.
  3. **Read it back, decrypted:** IMAPS AUTH PLAIN → SELECT INBOX → UID FETCH
     BODY[] returns the message *decrypted* (the MDA opens it server-side from the
     snapshot it unwrapped at AUTH), with the Subject + body byte-present.

**Scan gate.** ``internal/mta/scan_gate.go`` is default-on + fail-closed: an
unreachable clamd/rspamd 451s *every* inbound DATA (and hangs ~30s per scanner on
the dial), and there is no disable knob today (``ScanPolicyDefault`` is
compile-time). The nest container can't reach host listeners on this docker
setup, so we run the proven ``fakes/{fake_clamd,fake_rspamd}`` as **sidecar
containers on a user-defined network** the nest joins, reached by container name
— the docker-compose-equivalent the plan anticipated, via plain ``docker run`` +
a mounted python image (not the real, slow, DB-downloading ``clamav``/``rspamd``
images).

**Delivery + perimeter (Gap 2g prod parity — ``testing.md`` § Gap 2 Target).**
Inbound is delivered from *inside* the container over loopback
(``deliver_inbound_loopback_curl``): the DNS-dependent HELO-identity + FCrDNS
checks are loopback-exempt, so a synthetic sender clears them exactly as the
proven process-level ``test_mail_bridge_mta.py`` round-trip does. But the box runs
the **live-box posture**, not the relaxed opt-out: storage mode **Encrypted**
(example.com's default) and the inbound AUTH perimeter **ENFORCED**
(``enforce_dmarc=true``/``log_only=false``) via the shared ``enforce_mail_perimeter``
+ ``publish_passing_mail_dns`` helpers. The sender ``external-sender@sender.test``
publishes a passing SPF (``ip4:127.0.0.1`` — the loopback client IP) + ``_dmarc …
p=reject`` through a fake_dns sidecar (the nest's ``--dns``), so acceptance is
earned by an aligned DMARC pass, not a no-policy pass — the receive half of the
canonical round-trip now runs the same enforced gate the live box does (the exact
class DMARC-enforcement bugs belong to). The spam gate stays relaxed
for the synthetic loopback peer (DNSBL/FCrDNS are real-PTR/RBL gates it can't
satisfy — orthogonal to the AUTH gate). The mail seal path is storage-mode-independent
(msek-gated, not ``StorageMode``-gated), so Encrypted exercises
identical mail code.
"""

import os
import subprocess

import pytest


from .helpers import (
    IMAGE_TAG,
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
    provision_mail_recipient,
    publish_passing_mail_dns,
    remove_container,
    remove_network,
    start_container_with_ports,
    start_fake_dns_sidecar,
    start_fake_scanner_sidecars,
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

DOMAIN = "localhost"               # the deployment's primary (local) mail domain
SENDER_DOMAIN = "sender.test"      # the inbound sender's domain (publishes passing SPF + _dmarc)
CLAIM_CODE = "RNDTR5"
RECIPIENT_LOCAL = "inbox-user"
RECIPIENT_PASSWORD = "round-trip-password-1"


# ── Fixtures ──────────────────────────────────────────────────────────


@pytest.fixture(scope="module")
def docker_image():
    """Build the image once for this module (shared tag, layer-cached)."""
    docker_build(get_repo_root())
    yield IMAGE_TAG


@pytest.fixture()
def scanners(docker_image, tmp_path):
    """Fake clamd + rspamd + a ``records_dir``-backed fake_dns resolver as sidecar
    containers on a fresh user-defined network the nest joins. The scan gate is
    fail-closed and the nest container can't reach host listeners on this docker
    setup, so the scanners must live *inside* the nest's network (the nest dials
    them by container name). The fake_dns sidecar (the nest's ``--dns``) lets the
    inbound sender domain publish a PASSING SPF + ``_dmarc … p=reject`` for the
    ENFORCED DMARC perimeter (Gap 2g prod parity — ``testing.md`` § Gap 2 Target).
    Yields {network, clamd_addr, rspamd_url, dns_ip, records_dir}."""
    suffix = find_free_port()  # unique-enough token for the network + container names
    network = f"fauna-rt-net-{suffix}"
    clamd_name = f"fauna-rt-clamd-{suffix}"
    rspamd_name = f"fauna-rt-rspamd-{suffix}"
    dns_name = f"fauna-rt-dns-{suffix}"
    fakes_dir = str(get_repo_root() / "tests" / "e2e-unified" / "fakes")
    records_dir = str(tmp_path)
    os.chmod(records_dir, 0o777)  # the fake_dns sidecar reads it through a bind mount
    create_network(network)
    try:
        addrs = start_fake_scanner_sidecars(
            network, fakes_dir, clamd_name=clamd_name, rspamd_name=rspamd_name)
        dns_ip = start_fake_dns_sidecar(
            network, fakes_dir, name=dns_name, txt_records={}, records_dir=records_dir)
        yield {"network": network, "dns_ip": dns_ip, "records_dir": records_dir, **addrs}
    finally:
        remove_container(clamd_name)
        remove_container(rspamd_name)
        remove_container(dns_name)
        remove_network(network)


@pytest.fixture()
def round_trip_nest(docker_image, scanners):
    """Fresh claimed container on the scanners' network with all four mail ports
    mapped, the scan gate pointed at the sidecar fakes by container name, and the
    resolver (``--dns``) pointed at the fake_dns sidecar so the inbound sender
    domain resolves a PASSING SPF + ``_dmarc`` for the enforced perimeter (Gap 2g —
    ``testing.md`` § Gap 2 Target). Storage mode committed **Encrypted** —
    example.com's privacy-respecting default and the goal-doc prod-parity axis; certs
    still provision via the self-signed boot bootstrap (same as the Encrypted
    bidirectional/two-nest/factory-reset tests), and the mail seal/store/IMAP-decrypt
    path is storage-mode-independent (msek-gated, not ``StorageMode``-gated). Cleaned up after."""
    http_port, *mail = find_free_ports(5)
    mail_ports = dict(zip((25, 465, 587, 993), mail))  # container_port -> host_port
    name = f"fauna-mail-roundtrip-{http_port}"
    start_container_with_ports(
        name,
        {3000: http_port, **mail_ports},
        env={
            "FAUNA_CLAIM_CODE": CLAIM_CODE,
            "FAUNA_PORT": "3000",
            # Fail-closed scan gate → point the bridge's operator-hatch at the
            # sidecar scanners, reachable by container name on the shared network.
            "FAUNA_CLAMD_ADDR": scanners["clamd_addr"],
            "FAUNA_RSPAMD_URL": scanners["rspamd_url"],
        },
        # The resolver points at the fake_dns sidecar so `verify_inbound` reads the
        # sender domain's published SPF/_dmarc for the ENFORCED DMARC gate (Gap 2g);
        # the loopback delivery is still exempt from the reachability-gated
        # HELO/FCrDNS checks.
        dns=scanners["dns_ip"],
        network=scanners["network"],
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
            "records_dir": scanners["records_dir"],
        }
    finally:
        remove_container(name)


# ── Test ──────────────────────────────────────────────────────────────


@pytest.mark.feature("mail-server")
def test_mail_deploy_inbound_round_trip(round_trip_nest, run_seal_helper):
    nest = round_trip_nest
    name = nest["name"]
    mp = nest["mail_ports"]  # container_port -> host_port
    records_dir = nest["records_dir"]

    # nest-side setup before bring-up, via the shared Gap-2g prod-parity helper:
    # register the primary mail domain (serving ordering — see the lifecycle test
    # docstring), relax ONLY the orthogonal spam gates a synthetic peer can't satisfy,
    # and ENFORCE the DMARC perimeter (`enforce_dmarc=true`, `log_only=false`). The
    # peer is loopback (`deliver_inbound_loopback_curl`), so `cross_container_peer`
    # stays False — HELO identity is loopback-exempt, but the enforced DMARC gate
    # still fires (proven by the `550` negative control in `test_mail_security_accept.py`),
    # so the perimeter is non-vacuous. Then provision the recipient as a primary
    # client would (MSEK-derived recipient pubkey + wrapped-MSEK AUTH credential +
    # MLS snapshot, all keyed off one MSEK so the seal/open halves match).
    enforce_mail_perimeter(nest, domain=DOMAIN)
    recipient = provision_mail_recipient(
        nest, run_seal_helper, domain=DOMAIN, local_part=RECIPIENT_LOCAL,
        password=RECIPIENT_PASSWORD,
    )

    # Publish the inbound sender domain's PASSING perimeter into the fake_dns: a
    # passing SPF authorizing the loopback (`spf_ip=127.0.0.1` — the loopback
    # delivery connects from inside the container, so the MTA sees 127.0.0.1 as the
    # client IP) + `_dmarc … p=reject`. Under the enforced gate above, the inbound is
    # accepted ONLY because it DMARC-passes via the aligned SPF pass (From:/envelope
    # both on `sender.test` ⇒ aspf-aligned) — the proven `test_mail_security_accept.py`
    # loopback-accept path, now exercised as the inbound leg of the round-trip.
    publish_passing_mail_dns(records_dir, {SENDER_DOMAIN: "127.0.0.1"}, spf_ip="127.0.0.1")

    bring_bridges_to_serving(name, nest, mp, DOMAIN)

    # (receive) Deliver one unauthenticated inbound message over STARTTLS on 25,
    # from inside the container over loopback (see deliver_inbound_loopback_curl
    # for why: the DNS-dependent HELO/FCrDNS perimeter checks are loopback-exempt;
    # a host client would arrive as the non-loopback bridge gateway and be
    # rejected). The sender is `external-sender@sender.test`, whose published passing
    # SPF (ip4:127.0.0.1) + `_dmarc p=reject` clear the ENFORCED DMARC gate via the
    # aligned SPF pass — so the receive leg runs the full prod-parity AUTH perimeter,
    # not the relaxed pass. The MTA validates the recipient (alias → actor + MLS
    # pubkey), runs the scan gate (clean via the host fakes), HPKE-seals the body to
    # the recipient's MSEK-derived pubkey, and ingests it to INBOX (Accept).
    subject = "Stage-5 inbound round-trip"
    body_marker = "deploy-image inbound round-trip"
    msg_id = deliver_inbound_loopback_curl(
        name,
        mail_from=f"external-sender@{SENDER_DOMAIN}",
        rcpt_to=recipient["username"],
        subject=subject,
        body_text=f"Hello from the {body_marker} test.\n",
    )

    # (read back, decrypted) IMAPS AUTH PLAIN → SELECT INBOX → UID FETCH BODY[].
    # The MDA AEAD-unwraps the wrapped MSEK under the password at AUTH, Decrypts
    # the snapshot, and OpenMailRecord-opens the body with its leaf secret — so
    # the client receives plaintext. Same MSEK on both ends ⇒ the open succeeds.
    try:
        raw = imap_fetch_only_inbox_message(
            "127.0.0.1", mp[993], DOMAIN,
            recipient["username"], recipient["password"],
        )
    except (AssertionError, TimeoutError) as e:
        raise AssertionError(f"{e}\n\n── bridge diagnostics ──\n{bridge_diag(name)}") from e

    text = raw.decode("utf-8", errors="replace")
    assert subject in text, (
        f"decrypted body must carry the Subject; got first 400B: {text[:400]!r}"
    )
    assert body_marker in text, (
        f"decrypted body must carry the message text; got first 400B: {text[:400]!r}"
    )
    assert msg_id.strip("<>") in text, "decrypted body must carry the Message-ID"

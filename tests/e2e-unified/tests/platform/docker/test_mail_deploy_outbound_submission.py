"""tier_4 e2e: the full outbound submission round-trip in the real Docker deploy image.

Stage 5 **Slice 6** of the mail-deployment VPS milestone
(tracked internally). The symmetric
counterpart of ``test_mail_deploy_inbound_round_trip.py``: that test proves the
*receive* half of the milestone's Gmail bar (external → IMAP read, decrypted);
this one proves the *send* half — an authenticated user submits a message
through the deployed submission listener, the nest DKIM-signs it at the outbound
hand-out, and the MTA relays it out to an external MX, end-to-end through the packaged binaries.

This is the ``green-test-or-it-doesn't-work`` deliverable for send: the lifecycle
test asserts only that 587 serves a *220 banner*, which proves the listener
binds, not that a user can authenticate + get a signed message delivered. Each
sub-step is proven at the process level (``test_mail_bridge_mta.py``'s five
submission tests — AUTH, DKIM-sign, MX-relay via a stub), but only against the
in-process bridge fixture; this glues AUTH → sign → relay-out in the **real
image** with a **real submission identity**, the seam the deploy image never
exercised.

What it asserts:
  1. Bring the deploy image's bridges to *serving* (``bring_bridges_to_serving``).
  2. Nothing is provisioned for DKIM: the nest minted the primary domain's key
     when the domain was registered, and signs at the outbound hand-out.
  3. **Submit:** SASL PLAIN AUTH over implicit TLS on 465 (the wrapped submission
     token, AEAD-unsealed + inner-Ed25519-verified bridge-side) → MAIL/RCPT/DATA.
  4. **Relay out, signed:** the message arrives at a stub external MX (a sidecar
     container the MTA reaches by name via the operator-hatch ``mta_mx_override``)
     bearing a ``DKIM-Signature`` whose ``d=`` aligns with the From: domain — the
     in-CI proxy for Gmail ``dkim=pass`` + DMARC alignment.

**Outbound route.** The MTA's outbound worker resolves the recipient domain's
next hop via ``OverrideMXResolver`` when the operator-hatch ``mta_mx_override``
names it (config.go § mta_mx_override — a real split-horizon / air-gapped relay
affordance). The deploy image exposes it via ``FAUNA_MTA_MX_OVERRIDE`` (entrypoint
``operator-hatch.toml``, parallel to the clamd/rspamd scanner env), here pointing
``external.test`` at the stub-MX sidecar by container name on a shared user net.
It persists each delivery to a mounted dir the host test reads. Submission is
authenticated and has *no* inbound perimeter (no HELO/FCrDNS/greylist), so it's
driven from the host over the mapped 465 port — unlike the inbound test's
loopback-from-inside delivery.

**Relay transport (parametrized).** The test runs twice, over the relay leg's
transport security:
  - ``plaintext``: the stub does not advertise STARTTLS → the bridge's
    opportunistic-STARTTLS sender falls back to cleartext (no public CA chain
    needed). The original Slice-6 relay proof.
  - ``starttls``: the stub advertises STARTTLS and refuses cleartext mail-flow →
    a delivery proves the bridge upgraded the relay leg to TLS (RFC 7435
    opportunistic security, ``InsecureSkipVerify`` against the self-signed stub
    cert), the in-CI proxy for the real-Gmail TLS leg.

**Prod-parity posture (Gap 2g — ``testing.md`` § Gap 2 Target): deliberate opt-out.**
The Gap-2g default makes the enforced inbound DMARC perimeter the standard
tier_4 shape, with ``relax_spam_policy`` an explicit per-test opt-OUT. This
test legitimately opts out of it, because its flow doesn't exercise the
inbound perimeter axis at all: authenticated submission has **no inbound
perimeter** (no HELO/FCrDNS/greylist/DMARC gate — the bridge binds the
envelope sender to the AUTH'd identity), so an ``enforce_mail_perimeter`` call
would set a gate that never fires. The message also relays straight out to the
stub MX with **no read-back of a sealed copy**, so the storage seal/decrypt
path is not exercised here either — that combination (DKIM-signed outbound
submission WITH a sealed-copy read-back) is already covered by
``test_mail_deploy_bidirectional_round_trip.py``'s send half. (No-modes
retirement, ratified 2026-07-12: every nest is sealed at rest unconditionally
now, so there is no longer a storage-mode axis to flip between the two tests
either — the division of labor above is unchanged, just no longer phrased as
"Encrypted vs plaintext".) So this test stays focused on the pure
submission→sign→relay-out seam; the enforced-perimeter fidelity gain lives in
the inbound/round-trip tests.
"""

import shutil
import ssl
import tempfile
import time

import pytest

from helpers.mail_wire import dkim_signature_tag

from .helpers import (
    IMAGE_TAG,
    bridge_diag,
    bring_bridges_to_serving,
    claim_admin_api,
    create_network,
    docker_build,
    find_free_port,
    find_free_ports,
    get_repo_root,
    provision_mail_sender,
    read_stub_mx_message,
    register_primary_domain,
    remove_container,
    remove_network,
    start_container_with_ports,
    start_stub_mx_sidecar,
    submit_message_tls,
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

DOMAIN = "localhost"          # the deployment's primary (local) mail domain
EXTERNAL_DOMAIN = "external.test"   # the relayed-to domain (mapped to the stub MX)
CLAIM_CODE = "OUTBND"
SENDER_LOCAL = "sender"
SENDER_PASSWORD = "outbound-submission-password-1"


# ── Fixtures ──────────────────────────────────────────────────────────


@pytest.fixture(scope="module")
def docker_image():
    """Build the image once for this module (shared tag, layer-cached)."""
    docker_build(get_repo_root())
    yield IMAGE_TAG


@pytest.fixture(params=[False, True], ids=["plaintext", "starttls"])
def stub_mx(request, docker_image):
    """Stub external MX as a sidecar container on a fresh user-defined network the
    nest joins; the MTA's outbound worker relays to it by container name. The host
    test reads deliveries from the mounted out-dir (the sidecar's in-memory list is
    cross-process-unreachable). Yields {network, target, out_dir, require_starttls}.

    Parametrized over the relay's transport security:
      - ``plaintext``: the stub does not advertise STARTTLS → the bridge's
        opportunistic sender falls back to cleartext (the Slice-6 relay proof).
      - ``starttls``: the stub advertises STARTTLS and refuses cleartext mail-flow
        → a successful delivery proves the bridge upgraded the connection to TLS
        (RFC 7435 opportunistic security), the in-CI proxy for the real-Gmail TLS
        leg (Slice-6 STARTTLS hardening)."""
    require_starttls = request.param
    suffix = find_free_port()  # unique-enough token for the network + container names
    network = f"fauna-out-net-{suffix}"
    stub_name = f"fauna-out-stubmx-{suffix}"
    helpers_dir = str(get_repo_root() / "tests" / "e2e-unified" / "helpers")
    out_dir = tempfile.mkdtemp(prefix="fauna-stubmx-")
    # The sidecar (container root, possibly userns-remapped) writes deliveries
    # here through a bind mount; make it world-writable so the write succeeds
    # regardless of the daemon's uid-remap policy.
    import os as _os

    _os.chmod(out_dir, 0o777)
    create_network(network)
    try:
        target = start_stub_mx_sidecar(
            network, helpers_dir, out_dir, name=stub_name, require_starttls=require_starttls)
        yield {"network": network, "target": target, "out_dir": out_dir,
               "require_starttls": require_starttls}
    finally:
        remove_container(stub_name)
        remove_network(network)
        shutil.rmtree(out_dir, ignore_errors=True)


@pytest.fixture()
def outbound_nest(docker_image, stub_mx):
    """Fresh claimed container on the stub MX's network with all four mail ports
    mapped + ``external.test`` routed to the stub via ``FAUNA_MTA_MX_OVERRIDE``.
    Storage mode committed (plaintext — the "I trust the box" deploy default;
    needed for cert provisioning). Cleaned up after."""
    http_port, *mail = find_free_ports(5)
    mail_ports = dict(zip((25, 465, 587, 993), mail))  # container_port -> host_port
    name = f"fauna-mail-outbound-{http_port}"
    start_container_with_ports(
        name,
        {3000: http_port, **mail_ports},
        env={
            "FAUNA_CLAIM_CODE": CLAIM_CODE,
            "FAUNA_PORT": "3000",
            # Static outbound route: relay external.test to the stub-MX sidecar
            # (reached by container name on the shared net) instead of public MX.
            "FAUNA_MTA_MX_OVERRIDE": f"{EXTERNAL_DOMAIN}={stub_mx['target']}",
        },
        network=stub_mx["network"],
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
            "out_dir": stub_mx["out_dir"],
            "require_starttls": stub_mx["require_starttls"],
        }
    finally:
        remove_container(name)


# ── Test ────────────────────────────────────────────────────────────────


@pytest.mark.feature("mail-server")
def test_mail_deploy_outbound_submission(outbound_nest, run_seal_helper):
    nest = outbound_nest
    name = nest["name"]
    mp = nest["mail_ports"]  # container_port -> host_port
    out_dir = nest["out_dir"]

    # nest-side setup before bring-up: register the primary domain (serving
    # ordering — see the lifecycle test docstring) + provision the submission
    # identity (sender actor + alias + MLS pubkey + wrapped submission token).
    register_primary_domain(nest, DOMAIN)
    sender = provision_mail_sender(
        nest, run_seal_helper, domain=DOMAIN, local_part=SENDER_LOCAL,
        credential=SENDER_PASSWORD,
    )

    bring_bridges_to_serving(name, nest, mp, DOMAIN)

    # (submit) authenticated implicit-TLS submission on 465, From: == the
    # authenticated identity (the bridge binds the envelope sender to it).
    token = f"out-{int(time.time() * 1000)}"
    sender_addr = sender["username"]
    rcpt = f"recipient@{EXTERNAL_DOMAIN}"
    raw_message = (
        f"From: Sender <{sender_addr}>\r\n"
        f"To: {rcpt}\r\n"
        f"Subject: Stage-5 outbound submission {token}\r\n"
        f"Message-ID: <{token}@{DOMAIN}>\r\n"
        "Date: Sun, 25 May 2026 12:00:00 +0000\r\n"
        "MIME-Version: 1.0\r\n"
        "Content-Type: text/plain; charset=utf-8\r\n"
        "\r\n"
        "Hello from the deploy-image outbound submission test.\r\n"
    ).encode()

    try:
        submit_message_tls(
            "127.0.0.1", mp[465], DOMAIN,
            sender=sender_addr, password=SENDER_PASSWORD, rcpt=rcpt,
            raw_message=raw_message,
        )
    except (AssertionError, OSError, ssl.SSLError) as e:
        raise AssertionError(
            f"submission failed: {e}\n\n── bridge diagnostics ──\n{bridge_diag(name)}") from e

    # (relay out, signed) the bridge enqueues the message, the nest signs it at
    # the hand-out, and the outbound worker drains it immediately (submission
    # triggers a poll); it lands
    # at the stub external MX over the mta_mx_override route. In the `starttls`
    # mode the stub refuses cleartext mail-flow, so a delivery here additionally
    # proves the bridge upgraded the relay leg to TLS (opportunistic STARTTLS).
    received = read_stub_mx_message(out_dir, token, timeout=30.0)
    tls_note = (
        " (the stub requires STARTTLS, so non-delivery means the bridge did NOT "
        "upgrade the relay leg to TLS)" if nest["require_starttls"] else ""
    )
    assert received is not None, (
        f"stub external MX received no message tagged {token} within 30s — outbound "
        f"delivery did not complete{tls_note}.\n\n── bridge diagnostics ──\n{bridge_diag(name)}"
    )
    assert b"dkim-signature:" in received.lower(), (
        f"delivered message has no DKIM-Signature header — the nest handed out "
        f"unsigned mail. First 600 bytes:\n{received[:600]!r}"
    )
    # DMARC alignment proxy: the signature's d= equals the From: domain.
    d_tag = dkim_signature_tag(received, "d")
    assert d_tag == DOMAIN, (
        f"DKIM-Signature d= must align with the From: domain {DOMAIN!r} "
        f"(Gmail dkim=pass + DMARC-aligned proxy); got d={d_tag!r}"
    )

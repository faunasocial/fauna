"""Live, opt-in end-to-end test: the home nest behind a public relay REFUSES a
certificate delivered through the relay by anyone but one of its own admins
(``docs/features/home-nest-behind-a-relay.md`` outcome 12;
``docs/goal/architecture/nest/tls-certificates.md`` § B, *Who may sign it: an
admin of the installing nest — on both routes*).

Why live, not docker: the home box dials a user's relay only from that user's
OWN home-side pairing row (``nest_sync_worker.rs::run_sync_cycle``), and a row
whose actor is not an admin of the home box may name only a GLOBAL address
carrying a WebPKI-valid cert (``federation_pool.rs::discovery_dial_admits``).
Every docker address is non-global and every docker cert self-signed, so a
non-admin's relayed entry is never pulled there at all — a docker test would
pass without the refusal ever running. Here the relay is a real Hetzner VPS at
a real FQDN with an ACME cert, so the pull happens and the installer's own
admin check (``lan_cert.rs::install_client_issued_cert``) is what refuses.

This is a ``[nest]`` outcome, so the witness launches NO app driver
(``feature-catalog.md`` § The two surfaces): everything is observed from
outside, and every gesture goes over the wire or through the harness library.
Two identities:

  * **B — the relay's admin, a mere user at home.** B provisions the relay
    through the shared onboarding machine every app's wizard runs, driven
    app-less by the harness library (``provision_app_less`` → the
    ``e2e-harness`` flavor's ``fauna_harness_provision``, the same calls
    ``test_hetzner_provision.py`` sends an app), so the run's own claim makes
    B the relay's admin at once. A second admin granted through
    ``fauna.admin.admins.add`` would wait out its delay (24 h,
    ``pending_actions.rs``), which a release image cannot shorten, so the
    claim is the one immediate way. ``fauna.tls.publish_cert`` is Admin-class
    at the relay's door, so B is exactly the actor who can relay a certificate.
  * **A — the home box's admin.** A fresh identity claims the home container
    (production ``ghcr.io/faunasocial/nest:latest``, pulled, never built;
    ``FAUNA_MODE=private``) with the anonymous claim RPC and admits B with
    ``fauna.admin.users.create``, so B is a user of the home box and nothing
    more.

Then B adds the two pairing rows an app's both-ends link writes (``fauna.pair.add``
on each box: B's home-side row names ``https://<fqdn>``, the target the home
worker dials) and publishes a certificate sealed to the home box
(``fauna_ffi.build_publish_cert`` — the shared ``seal_issued_cert_for_nest``
every app's delivery calls) on the relay. The assertions:

  * the home box logs ``client-issued TLS cert: signer <B> is not an admin of
    this nest, skipping`` — the latency-independent anchor that the entry was
    PULLED and REFUSED (it is logged only on the pull path, after the dial);
  * the home listener still does not serve B's certificate.

The installing half — an admin's certificate through the relay IS installed —
is outcome 11, witnessed in docker
(``test_mail_relay_two_nest.py::test_a_certificate_for_the_home_nest_reaches_it_through_the_relay``),
where the one admin of both boxes is a loopback/admin-configured peer. It is
not repeated here: it needs one identity admin of BOTH boxes alongside B, which
on a release image is the 24 h grant above.

No mail, and no live sender box: unlike ``test_private_relay_hetzner.py`` this
module carries no ``FAUNA_LIVE_NEST_URL`` / ``live_box`` dependency.

Gating: ``tests/live/conftest.py``'s live-provision gate (the Hetzner cloud +
DNS tokens and ``FAUNA_E2E_LIVE=1``; real € cost) and Docker. Teardown of the
VPS + DNS is the ``box`` fixture's (always runs); the home container is removed
in this test's ``finally``.
"""
from __future__ import annotations

import os
import subprocess

import pytest
from nacl.signing import SigningKey

from clients._ws_rpc_core import RpcCallError
from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.live_provision import (
    hetzner_token,
    provision_app_less,
    retry_get,
)
from helpers.waiting import wait_until
from tests.live.test_private_relay_hetzner import (
    ghcr_latest_image,  # noqa: F401 — the module-scoped pull fixture, shared
)
from tests.platform.docker.helpers import (
    add_user_pairing,
    claim_admin_api,
    find_free_ports,
    node_id,
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
    pytest.mark.tier_4,
    pytest.mark.live_provisioning,
    # No client platform marker and no `app` fixture: the witness is app-less.
    pytest.mark.skipif(not HAS_DOCKER, reason="Docker not available"),
]

HOME_CLAIM_CODE = "RLYCRT"

# What an app's both-ends link grants when the user names no capabilities
# (`fauna_protocol::pair::default_self_sync`).
PAIR_CAPS = ["mls_pull", "namespace_sync", "post_forward", "mail_pull",
             "nostr_push", "account_replica"]

# The certificate's hostname: B's stand-in for a CA-issued cert. The relay
# route carries whatever it is handed; what matters is who signed it.
CERT_HOST = "home.example.com"

# The home worker's sync cycle runs every few seconds; a generous ceiling, not
# an expected duration — a green run pays only its poll interval.
REFUSAL_BUDGET_S = 300.0


def _self_signed(hostname: str) -> tuple[str, str, bytes]:
    """(cert PEM, key PEM, cert DER) for ``hostname``."""
    import datetime

    from cryptography import x509
    from cryptography.hazmat.primitives import hashes, serialization
    from cryptography.hazmat.primitives.asymmetric import ec
    from cryptography.x509.oid import NameOID

    key = ec.generate_private_key(ec.SECP256R1())
    name = x509.Name([x509.NameAttribute(NameOID.COMMON_NAME, hostname)])
    now = datetime.datetime.now(datetime.timezone.utc)
    cert = (x509.CertificateBuilder().subject_name(name).issuer_name(name)
            .public_key(key.public_key()).serial_number(x509.random_serial_number())
            .not_valid_before(now - datetime.timedelta(minutes=5))
            .not_valid_after(now + datetime.timedelta(days=30))
            .add_extension(x509.SubjectAlternativeName([x509.DNSName(hostname)]), False)
            .sign(key, hashes.SHA256()))
    key_pem = key.private_bytes(serialization.Encoding.PEM,
                                serialization.PrivateFormat.PKCS8,
                                serialization.NoEncryption()).decode()
    return (cert.public_bytes(serialization.Encoding.PEM).decode(), key_pem,
            cert.public_bytes(serialization.Encoding.DER))


def _container_logs(name: str) -> str:
    proc = subprocess.run(["docker", "logs", name], capture_output=True,
                          text=True, timeout=60)
    return (proc.stdout or "") + (proc.stderr or "")


def _sync_diag(name: str) -> str:
    """The home worker's own account of its pulls — the lines that say whether
    it dialed the relay for B's row at all, and why not."""
    lines = [ln for ln in _container_logs(name).splitlines()
             if any(k in ln for k in ("sync", "pairing", "federation",
                                      "client-issued TLS cert", "dial"))]
    return "\n".join(lines[-60:]) or "(no sync/pairing/federation lines in the home log)"


@pytest.mark.feature("home-nest-behind-a-relay")
def test_the_home_nest_refuses_a_relayed_certificate_its_admins_did_not_sign(
        box, ghcr_latest_image):  # noqa: F811
    from cryptography.hazmat.primitives import serialization

    from fauna_ffi import build_publish_cert
    from helpers.tls_spki import served_leaf

    token, dns_token = hetzner_token(), hetzner_token(purpose="dns")

    # B — the relay's admin (the provisioning identity), a mere user at home.
    b_secret_hex = box.identity_secret_hex
    b_sk = SigningKey(bytes.fromhex(b_secret_hex))
    b_actor, b_hex = bytes(b_sk.verify_key), bytes(b_sk.verify_key).hex()
    # A — the home box's admin, a separate identity.
    a_sk = SigningKey(os.urandom(32))
    a_actor = bytes(a_sk.verify_key)

    # ── Phase 1: B provisions the PUBLIC relay on Hetzner, app-less ─────────
    print(f"\n[relay-cert] provisioning public relay {box.subdomain} as B ...")
    snap = provision_app_less(box, token, dns_token=dns_token)
    assert snap.get("overall") == "Succeeded", (
        f"provisioning did not succeed: overall={snap.get('overall')!r} "
        f"final_error={snap.get('final_error')!r} steps={snap.get('steps')!r}"
    )
    result = snap.get("result") or {}
    box.server_id = result.get("server_id")
    box.ipv4 = result.get("ipv4")
    box.fqdn = result.get("domain")
    assert box.server_id and box.ipv4 and box.fqdn, f"incomplete provisioning result: {result}"
    relay_url = f"https://{box.fqdn}"
    print(f"[relay-cert] provisioned: server={box.server_id} ip={box.ipv4} fqdn={box.fqdn}")

    # FQDN TLS gate — the relay must be at its global address with a WebPKI
    # cert before the home worker will dial it for a non-admin's row.
    retry_get(f"{relay_url}/api/v1/health", verify=True, timeout=15,
              attempts=240, delay=10)
    print("[relay-cert] relay serving at its FQDN with a WebPKI cert")
    # The run's own claim made B the relay's admin: Admin-class kinds answer.
    with WsRpcAdminClient(relay_url, actor_id=b_actor, signing_key=bytes(b_sk)) as relay:
        relay.call("fauna.admin.users.list", {})
    print("[relay-cert] B is the relay's admin (the provisioning run's claim)")

    # ── Phase 2: the HOME nest on this host, A claims it over the wire ──────
    (home_port,) = find_free_ports(1)
    home_name = f"fauna-relay-cert-home-{home_port}"
    home_url = f"https://127.0.0.1:{home_port}"
    start_container_with_ports(
        home_name,
        {443: home_port},
        env={
            "FAUNA_MODE": "private",
            "FAUNA_CLAIM_CODE": HOME_CLAIM_CODE,
            "FAUNA_PORT": "3000",
        },
        image=ghcr_latest_image,
    )
    try:
        wait_for_health(home_port, home_name)
        print(f"[relay-cert] home nest up: {home_name} → {home_url}")
        claim_admin_api(home_port, HOME_CLAIM_CODE, handle="home", signing_key=a_sk)
        home = {"url": home_url}

        # ── Phase 3: A admits B on the home box — a user, not an admin ──────
        with WsRpcAdminClient(home_url, actor_id=a_actor, signing_key=bytes(a_sk)) as admin:
            admin.call("fauna.admin.users.create", {
                "actor_id": b_actor, "tier": "free", "label": "relay-admin", "handle": "relay-admin",
            })
        # B is a plain user there: an Admin-class kind refuses B.
        with WsRpcAdminClient(home_url, actor_id=b_actor, signing_key=bytes(b_sk)) as b_home:
            with pytest.raises(RpcCallError) as refused:
                b_home.call("fauna.admin.users.list", {})
        assert refused.value.code == "fauna.admin.permission_denied", refused.value
        print("[relay-cert] B admitted on the home box as a user (an admin kind refuses B)")

        # ── Phase 4: B's two pairing rows — the rows a both-ends link writes ─
        home_id, relay_id = node_id(home), node_id({"url": relay_url})
        add_user_pairing({"url": relay_url}, actor_id=b_actor, signing_key=bytes(b_sk),
                         other_nest_id=home_id, capabilities=PAIR_CAPS,
                         nest_url=home_url)
        add_user_pairing(home, actor_id=b_actor, signing_key=bytes(b_sk),
                         other_nest_id=relay_id, capabilities=PAIR_CAPS,
                         nest_url=relay_url)
        print("[relay-cert] B paired the relay and the home box (one row on each)")

        # ── Phase 5: B publishes a certificate sealed to the home box ───────
        cert_pem, key_pem, der = _self_signed(CERT_HOST)

        def served() -> bytes:
            return served_leaf("127.0.0.1", home_port, CERT_HOST).public_bytes(
                serialization.Encoding.DER)

        assert served() != der, "precondition: the home box cannot already serve B's cert"
        publish = build_publish_cert(cert_pem, key_pem, CERT_HOST, node_id(home),
                                     bytes(b_sk))
        with WsRpcAdminClient(relay_url, actor_id=b_actor, signing_key=bytes(b_sk)) as relay:
            reply = relay.call("fauna.tls.publish_cert", publish)
        assert reply.get("ok") is True, f"the relay must store B's entry; got {reply!r}"
        assert reply.get("installed") is False, (
            f"the relay cannot open a cert sealed to the home box; got {reply!r}")
        print("[relay-cert] B published a cert sealed to the home box on the relay")

        # ── Phase 6: pulled, refused, never served ──────────────────────────
        refusal = (f"client-issued TLS cert: signer {b_hex} is not an admin "
                   f"of this nest, skipping")
        wait_until(lambda: refusal in _container_logs(home_name), REFUSAL_BUDGET_S,
                   interval=3.0, diagnose=lambda: (
                       f"the home box never logged the refusal of B's relayed "
                       f"entry — it was not pulled, or not refused.\n"
                       f"── home worker log ──\n{_sync_diag(home_name)}"))
        assert served() != der, (
            "the home box must never serve a certificate a non-admin signed")
        print("[relay-cert] home box pulled B's entry, refused it, serves its own cert")
    finally:
        remove_container(home_name)

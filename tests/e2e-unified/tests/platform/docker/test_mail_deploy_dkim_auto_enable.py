"""tier_4 e2e: the nest holds the DKIM key and signs outbound mail, with no admin step.

DKIM signing is provisioned **automatically nest-side**: the nest mints an Ed25519
key for a mail domain when the domain is added (and at boot for one found without a
key), keeps the private half sealed under its own key-encryption key, and signs each
outbound message as it hands the message to the MTA — *no admin clicks, no client*
(``mail-bridge-lifecycle.md`` § DKIM provisioning (automatic) →
*Custody moves to the nest*; ``bins/fauna-nest/src/mail_dkim_key.rs``). This is the
in-CI proof of that mechanism in the real deploy image: where the sibling
``test_mail_deploy_outbound_submission.py`` asserts only that a relayed message is
signed and aligned, this one reads the published record and verifies against it.

**Two distinct bars share the one (module-scoped) image build:**
  - ``test_nest_auto_provisions_dkim_signed_outbound`` — the nest mints + signs,
    and the relayed message bears a ``d=``-aligned ``DKIM-Signature``
    (header **presence** + DMARC alignment).
  - ``test_nest_dkim_signature_verifies_against_published_record`` — the relayed
    signature **cryptographically verifies** against the nest's stored
    ``public_dns_value`` (the ``b=`` ed25519 check), catching a
    publish-vs-key divergence that header-presence cannot. The deployment-level
    analogue of the example.com 2026-06-21 ``dkim=fail`` live miss
    (tracked internally (C); memory ``dkim-publish-goes-stale-silently``).

What ``test_nest_auto_provisions_dkim_signed_outbound`` asserts:
  1. Bring the deploy image's bridges to *serving*
     (``bring_bridges_to_serving``; the domain's key was minted when the domain
     was registered — no bridge takes part in it).
  2. **The nest holds the domain's DKIM key** — ``list_dkim_selectors``
     shows the primary domain's ``default`` selector carrying a real RFC-6376
     ``v=DKIM1; k=ed25519`` public TXT value.
  3. **Submit + relay, signed:** an authenticated submission on 465 relays to a stub
     external MX bearing a ``DKIM-Signature`` whose ``d=`` aligns with the From:
     domain — the Gmail ``dkim=pass`` + DMARC-alignment proxy, reached with no manual
     DKIM step at all.

**Seal-helper scope.** DKIM uses NO seal-helper — the nest holds the key.
The *submission sender identity* (``provision_mail_sender``) still uses the seal-helper
for its wrapped submission token; that is orthogonal scaffolding for the outbound
oracle (the sender is not the unit under test), identical to
``test_mail_deploy_outbound_submission.py``.
"""

import base64
import shutil
import ssl
import tempfile
import time

import dkim
import pytest
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
from cryptography.hazmat.primitives.serialization import Encoding, PublicFormat

from helpers.mail_wire import dkim_signature_tag

from .helpers import (
    IMAGE_TAG,
    admin_ws,
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

DOMAIN = "localhost"            # the deployment's primary (local) mail domain
EXTERNAL_DOMAIN = "external.test"   # the relayed-to domain (mapped to the stub MX)
CLAIM_CODE = "DKAUTO"
SENDER_LOCAL = "sender"
SENDER_PASSWORD = "dkim-auto-submission-pw-1"


def _dkim_selectors(nest, domain: str) -> list[dict]:
    """Selectors provisioned for ``domain`` via ``fauna.bridges.list_dkim_selectors``."""
    with admin_ws(nest) as admin:
        return admin.call("fauna.bridges.list_dkim_selectors", {"domain": domain})["selectors"]


def _dnsfunc_returning(selector: str, domain: str, public_dns_value: str):
    """A dkimpy ``dnsfunc`` that answers ``<selector>._domainkey.<domain>`` with
    ``public_dns_value`` and NXDOMAINs everything else — i.e. it stands in for the
    admin having published *exactly* the nest's stored TXT on the admin-dns page.
    dkimpy passes the query name as **bytes** with a trailing dot, so match on bytes."""
    want = f"{selector}._domainkey.{domain}.".encode()

    def _resolve(name, timeout=5):
        key = name if isinstance(name, bytes) else name.encode()
        return public_dns_value.encode() if key == want else b""

    return _resolve


def _dkim_verifies(received: bytes, public_dns_value: str) -> bool:
    """Cryptographically verify the received message's first ``DKIM-Signature``
    against ``public_dns_value`` as if it were the published TXT — the ``b=``
    ed25519 check that example.com failed live 2026-06-21. The selector + domain are
    read from the signature itself (``s=`` / ``d=``), so the published name matches
    what the signer actually claimed."""
    selector = dkim_signature_tag(received, "s")
    domain = dkim_signature_tag(received, "d")
    assert selector and domain, (
        f"received message has no parseable DKIM-Signature s=/d=: s={selector!r} d={domain!r}")
    return dkim.verify(
        received, dnsfunc=_dnsfunc_returning(selector, domain, public_dns_value))


# ── Fixtures ──────────────────────────────────────────────────────────


@pytest.fixture(scope="module")
def docker_image():
    """Build the image once for this module (shared tag, layer-cached)."""
    docker_build(get_repo_root())
    yield IMAGE_TAG


@pytest.fixture()
def stub_mx(docker_image):
    """Stub external MX as a sidecar on a fresh user-defined network the nest joins;
    the MTA's outbound worker relays to it by IP. The host test reads deliveries from
    the mounted out-dir. Plaintext-only (the DKIM-Signature assertion is transport-
    independent — STARTTLS hardening is covered by test_mail_deploy_outbound_submission)."""
    import os

    suffix = find_free_port()
    network = f"fauna-dkauto-net-{suffix}"
    stub_name = f"fauna-dkauto-stubmx-{suffix}"
    helpers_dir = str(get_repo_root() / "tests" / "e2e-unified" / "helpers")
    out_dir = tempfile.mkdtemp(prefix="fauna-dkauto-stubmx-")
    os.chmod(out_dir, 0o777)  # world-writable through the bind mount (uid-remap-safe)
    create_network(network)
    try:
        target = start_stub_mx_sidecar(network, helpers_dir, out_dir, name=stub_name)
        yield {"network": network, "target": target, "out_dir": out_dir}
    finally:
        remove_container(stub_name)
        remove_network(network)
        shutil.rmtree(out_dir, ignore_errors=True)


@pytest.fixture()
def auto_dkim_nest(docker_image, stub_mx):
    """Fresh claimed container on the stub MX's network with all four mail ports
    mapped + ``external.test`` routed to the stub via ``FAUNA_MTA_MX_OVERRIDE``.
    Storage committed plaintext (the deploy default). Cleaned up after."""
    http_port, *mail = find_free_ports(5)
    mail_ports = dict(zip((25, 465, 587, 993), mail))  # container_port -> host_port
    name = f"fauna-dkauto-{http_port}"
    start_container_with_ports(
        name,
        {3000: http_port, **mail_ports},
        env={
            "FAUNA_CLAIM_CODE": CLAIM_CODE,
            "FAUNA_PORT": "3000",
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
        }
    finally:
        remove_container(name)


# ── Shared bring-up ───────────────────────────────────────────────────


def _bring_up_and_relay(nest, run_seal_helper, token: str):
    """The bring-up both DKIM tier_4 tests share: register the primary domain
    (the nest mints its DKIM key) + a send-only submission identity, bring the
    bridges to serving, read the nest-held ``default`` selector, submit an
    authenticated 465 message tagged ``token``, and return
    ``(received_raw, default_selector)``.

    DKIM uses **no** seal-helper (the nest holds the key); the seal-helper is
    only the submission-sender scaffolding (orthogonal — see the module docstring)."""
    name = nest["name"]
    mp = nest["mail_ports"]  # container_port -> host_port
    out_dir = nest["out_dir"]

    # ── 1. Setup (scaffolding): primary domain + send-only submission identity, then
    # bring the bridges to serving. Registering the domain is what mints its DKIM
    # key, nest-side. NO DKIM call, NO seal-helper for DKIM.
    register_primary_domain(nest, DOMAIN)
    sender = provision_mail_sender(
        nest, run_seal_helper, domain=DOMAIN, local_part=SENDER_LOCAL,
        credential=SENDER_PASSWORD,
    )
    bring_bridges_to_serving(name, nest, mp, DOMAIN)

    # ── 2. The NEST holds the `default` DKIM key — minted when the domain was
    # registered, with no client and no admin call. Poll briefly rather than
    # read once. ─────────────────────────────────────────────────────────────
    deadline = time.monotonic() + 30.0
    default_sel = None
    while time.monotonic() < deadline and default_sel is None:
        default_sel = next(
            (s for s in _dkim_selectors(nest, DOMAIN) if s["selector"] == "default"), None)
        if default_sel is None:
            time.sleep(1.5)
    assert default_sel is not None, (
        "the nest holds NO `default` DKIM selector for the primary "
        "domain after the MTA reached serving — the nest should hold a key for every "
        "active mail domain (minted when the domain is added, or at boot). "
        f"selectors={_dkim_selectors(nest, DOMAIN)}\n"
        f"── bridge diagnostics ──\n{bridge_diag(name)}"
    )

    # ── 3. Submit (authenticated implicit-TLS submission on 465) → relay out. ────
    sender_addr = sender["username"]
    rcpt = f"recipient@{EXTERNAL_DOMAIN}"
    raw_message = (
        f"From: Sender <{sender_addr}>\r\n"
        f"To: {rcpt}\r\n"
        f"Subject: nest-auto DKIM outbound {token}\r\n"
        f"Message-ID: <{token}@{DOMAIN}>\r\n"
        "Date: Sun, 25 May 2026 12:00:00 +0000\r\n"
        "MIME-Version: 1.0\r\n"
        "Content-Type: text/plain; charset=utf-8\r\n"
        "\r\n"
        "Hello from the nest-auto-DKIM outbound test.\r\n"
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

    received = read_stub_mx_message(out_dir, token, timeout=30.0)
    assert received is not None, (
        f"stub external MX received no message tagged {token} within 30s — outbound "
        f"delivery did not complete.\n\n── bridge diagnostics ──\n{bridge_diag(name)}"
    )
    return received, default_sel


# ── Tests ─────────────────────────────────────────────────────────────


@pytest.mark.feature("mail-server")
def test_nest_auto_provisions_dkim_signed_outbound(auto_dkim_nest, run_seal_helper):
    nest = auto_dkim_nest
    token = f"dkauto-{int(time.time() * 1000)}"
    received, default_sel = _bring_up_and_relay(nest, run_seal_helper, token)

    # The auto-provisioned selector carries a real RFC-6376 ed25519 public TXT value.
    assert default_sel["public_dns_value"].startswith("v=DKIM1; k=ed25519; p="), (
        "the auto-provisioned selector must carry a real RFC-6376 ed25519 public TXT "
        f"value (for the admin to publish on admin-dns); got {default_sel!r}"
    )

    # The relayed message must bear a DKIM-Signature with d= aligned — the whole point:
    # the NEST holds a *working* signing key and signs with it.
    assert b"dkim-signature:" in received.lower(), (
        "the relayed message has no DKIM-Signature header — the nest-auto-provisioned "
        f"key did not sign outbound mail. First 600 bytes:\n{received[:600]!r}"
    )
    d_tag = dkim_signature_tag(received, "d")
    assert d_tag == DOMAIN, (
        f"DKIM-Signature d= must align with the From: domain {DOMAIN!r} (Gmail "
        f"dkim=pass + DMARC-aligned proxy); got d={d_tag!r}"
    )


@pytest.mark.feature("mail-server")
def test_nest_dkim_signature_verifies_against_published_record(auto_dkim_nest, run_seal_helper):
    """(C) The relayed signature **cryptographically verifies** against the nest's
    stored ``public_dns_value`` — the ``b=`` ed25519 check, end-to-end, on the real
    deploy image. This is the bar header-presence (``..._signed_outbound``) cannot
    reach: it catches a **publish-vs-key divergence** where the key the nest
    signs with does not correspond to the public the nest reports for
    publication. The matched-pair unit round-trip
    (``libs/fauna-mail/tests/dkim_tests.rs``) cannot catch this (it signs + verifies
    with one keypair by construction); only the real mint→store→sign→stored-public
    path on the deploy image can. Deployment-level analogue of the example.com
    2026-06-21 ``dkim=fail`` live miss (tracked internally (C))."""
    nest = auto_dkim_nest
    token = f"dkverify-{int(time.time() * 1000)}"
    received, default_sel = _bring_up_and_relay(nest, run_seal_helper, token)
    published = default_sel["public_dns_value"]

    # POSITIVE: the actual relayed signature verifies against the value the nest
    # reports for publication (`list_dkim_selectors` reads the same `mail_dkim_keys`
    # row the nest signs with — so a green here proves the sealed private half and the
    # stored public half are a matched pair, on the real image).
    assert _dkim_verifies(received, published), (
        "the relayed DKIM-Signature did NOT cryptographically verify against the nest's "
        f"stored public_dns_value {published!r} — the key the nest signs with "
        "does not match the public the nest reports for publication (the publish-vs-key "
        "divergence; the deployment-level example.com 2026-06-21 dkim=fail "
        f"class). Received head:\n{received[:600]!r}\n"
        f"── bridge diagnostics ──\n{bridge_diag(nest['name'])}"
    )

    # NEGATIVE control: the same message must FAIL against an unrelated published key,
    # proving the verify actually checks `b=` (guards against a vacuous always-True
    # green that would let a real divergence slip through).
    bogus_pub = Ed25519PrivateKey.generate().public_key().public_bytes(
        Encoding.Raw, PublicFormat.Raw)
    bogus_value = f"v=DKIM1; k=ed25519; p={base64.b64encode(bogus_pub).decode()}"
    assert not _dkim_verifies(received, bogus_value), (
        "DKIM verify returned True against an UNRELATED published key — the verification "
        "is vacuous (not actually checking the b= signature), so a real publish-vs-key "
        "divergence would slip through this gate."
    )

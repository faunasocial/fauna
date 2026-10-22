"""tier_4 e2e: the nest holds the DKIM key and signs outbound mail, with no admin step.

DKIM signing is provisioned **automatically nest-side**: the nest mints an RSA-2048 +
Ed25519 key pair for a mail domain when the domain is added (and at boot for one found
without them), keeps the private halves sealed under its own key-encryption key, and
signs each outbound message twice — ``s=default-rsa`` then ``s=default-ed25519``
(``mail-multidomain.md`` § Per-domain DKIM → *Key shape*) — as it hands the message
to the MTA — *no admin clicks, no client*
(``mail-bridge-lifecycle.md`` § DKIM provisioning (automatic) →
*Custody moves to the nest*; ``bins/fauna-nest/src/mail_dkim_key.rs``). This is the
in-CI proof of that mechanism in the real deploy image: where the sibling
``test_mail_deploy_outbound_submission.py`` asserts only that a relayed message is
signed and aligned, this one reads the published record and verifies against it.

**Two distinct bars share the one (module-scoped) image build:**
  - ``test_nest_auto_provisions_dkim_signed_outbound`` — the nest mints + signs,
    and the relayed message bears two ``d=``-aligned ``DKIM-Signature`` headers
    (header **presence** + DMARC alignment).
  - ``test_nest_dkim_signature_verifies_against_published_record`` — each relayed
    signature **cryptographically verifies** against the nest's stored
    ``public_dns_value`` for its selector (the RSA and Ed25519 ``b=`` checks), catching a
    publish-vs-key divergence that header-presence cannot. The deployment-level
    analogue of the live nest's 2026-06-21 ``dkim=fail`` live miss
    (tracked internally (C); memory ``dkim-publish-goes-stale-silently``).

What ``test_nest_auto_provisions_dkim_signed_outbound`` asserts:
  1. Bring the deploy image's bridges to *serving*
     (``bring_bridges_to_serving``; the domain's key was minted when the domain
     was registered — no bridge takes part in it).
  2. **The nest holds the domain's DKIM keys** — ``list_dkim_selectors``
     shows the primary domain's ``default-rsa`` and ``default-ed25519`` selectors
     carrying real RFC-6376 ``v=DKIM1; k=rsa`` / ``k=ed25519`` public TXT values.
  3. **Submit + relay, signed:** an authenticated submission on 465 relays to a stub
     external MX bearing two ``DKIM-Signature`` headers whose ``d=`` aligns with the From:
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

import pytest
from cryptography.hazmat.primitives.asymmetric import rsa
from cryptography.hazmat.primitives.asymmetric.ed25519 import Ed25519PrivateKey
from cryptography.hazmat.primitives.serialization import Encoding, PublicFormat

from helpers.mail_wire import (
    dkim_selector_pair,
    dkim_signature_tags,
    verify_each_dkim_signature,
)

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


def _bogus_records() -> dict[str, str]:
    """Unrelated RSA-2048 and Ed25519 public records under the two ``default``
    selectors — the negative control's stand-in for a stale publish."""
    rsa_pub = rsa.generate_private_key(public_exponent=65537, key_size=2048).public_key()
    ed_pub = Ed25519PrivateKey.generate().public_key()
    rsa_p = base64.b64encode(
        rsa_pub.public_bytes(Encoding.DER, PublicFormat.SubjectPublicKeyInfo)).decode()
    ed_p = base64.b64encode(ed_pub.public_bytes(Encoding.Raw, PublicFormat.Raw)).decode()
    rsa_selector, ed_selector = dkim_selector_pair("default")
    return {
        rsa_selector: f"v=DKIM1; k=rsa; p={rsa_p}",
        ed_selector: f"v=DKIM1; k=ed25519; p={ed_p}",
    }


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
        {443: http_port, **mail_ports},
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
    (the nest mints its DKIM keys) + a send-only submission identity, bring the
    bridges to serving, read the nest-held ``default`` label's two records,
    submit an authenticated 465 message tagged ``token``, and return
    ``(received_raw, {selector: public_dns_value})`` — ``default-rsa`` then
    ``default-ed25519``.

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

    # ── 2. The NEST holds the `default` label's two DKIM keys — minted when the
    # domain was registered, with no client and no admin call. Poll briefly
    # rather than read once. ─────────────────────────────────────────────────
    want = dkim_selector_pair("default")
    deadline = time.monotonic() + 30.0
    published: dict[str, str] = {}
    while time.monotonic() < deadline and list(published) != want:
        listed = {s["selector"]: s["public_dns_value"] for s in _dkim_selectors(nest, DOMAIN)}
        published = {s: listed[s] for s in want if s in listed}
        if list(published) != want:
            time.sleep(1.5)
    assert list(published) == want, (
        f"the nest holds NOT both of {want!r} for the primary domain after the MTA "
        "reached serving — the nest should hold an RSA + Ed25519 key pair for every "
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
    return received, published


# ── Tests ─────────────────────────────────────────────────────────────


@pytest.mark.feature("mail-server")
def test_nest_auto_provisions_dkim_signed_outbound(auto_dkim_nest, run_seal_helper):
    nest = auto_dkim_nest
    token = f"dkauto-{int(time.time() * 1000)}"
    received, published = _bring_up_and_relay(nest, run_seal_helper, token)

    # The auto-provisioned selectors carry real RFC-6376 public TXT values, one
    # per algorithm.
    for selector, k in zip(dkim_selector_pair("default"), ("rsa", "ed25519")):
        assert published[selector].startswith(f"v=DKIM1; k={k}; p="), (
            f"the auto-provisioned {selector!r} selector must carry a real RFC-6376 "
            f"{k} public TXT value (for the admin to publish on admin-dns); "
            f"got {published!r}"
        )

    # The relayed message must bear two DKIM-Signatures, RSA first, with d=
    # aligned — the whole point: the NEST holds *working* signing keys and signs
    # with both.
    s_tags = dkim_signature_tags(received, "s")
    assert s_tags == dkim_selector_pair("default"), (
        "the relayed message does not carry the two DKIM-Signatures (s=default-rsa, "
        f"then s=default-ed25519); got s={s_tags!r}. First 900 bytes:\n{received[:900]!r}"
    )
    d_tags = dkim_signature_tags(received, "d")
    assert d_tags == [DOMAIN, DOMAIN], (
        f"every DKIM-Signature d= must align with the From: domain {DOMAIN!r} (Gmail "
        f"dkim=pass + DMARC-aligned proxy); got d={d_tags!r}"
    )


@pytest.mark.feature("mail-server")
def test_nest_dkim_signature_verifies_against_published_record(auto_dkim_nest, run_seal_helper):
    """(C) BOTH relayed signatures **cryptographically verify**, each against the
    nest's stored ``public_dns_value`` for its selector — the ``b=`` check of the
    RSA signature (the one Gmail reads) and of the Ed25519 one, end-to-end, on the
    real deploy image. This is the bar header-presence (``..._signed_outbound``) cannot
    reach: it catches a **publish-vs-key divergence** where the key the nest
    signs with does not correspond to the public the nest reports for
    publication. The matched-pair unit round-trip
    (``libs/fauna-mail/tests/dkim_tests.rs``) cannot catch this (it signs + verifies
    with one keypair by construction); only the real mint→store→sign→stored-public
    path on the deploy image can. Deployment-level analogue of the live nest's
    2026-06-21 ``dkim=fail`` live miss (tracked internally (C))."""
    nest = auto_dkim_nest
    token = f"dkverify-{int(time.time() * 1000)}"
    received, published = _bring_up_and_relay(nest, run_seal_helper, token)

    # POSITIVE: each relayed signature verifies against the value the nest reports
    # for publication under its selector (`list_dkim_selectors` reads the same
    # `mail_dkim_keys` rows the nest signs with — so a green here proves each sealed
    # private half and its stored public half are a matched pair, on the real image).
    verified, queries = verify_each_dkim_signature(received, DOMAIN, published)
    assert verified == list(published), (
        f"only {verified!r} of the relayed DKIM-Signatures cryptographically verified "
        f"against the nest's stored public_dns_values {published!r} — a key the nest "
        "signs with does not match the public the nest reports for publication (the "
        "publish-vs-key divergence; the deployment-level live-nest 2026-06-21 dkim=fail "
        f"class). dnsfunc queries: {queries!r}. Received head:\n{received[:900]!r}\n"
        f"── bridge diagnostics ──\n{bridge_diag(nest['name'])}"
    )

    # NEGATIVE control: the same message must FAIL against unrelated published keys,
    # proving the verify actually checks each `b=` (guards against a vacuous
    # always-True green that would let a real divergence slip through).
    verified, _ = verify_each_dkim_signature(received, DOMAIN, _bogus_records())
    assert verified == [], (
        f"DKIM verify passed {verified!r} against UNRELATED published keys — the "
        "verification is vacuous (not actually checking the b= signature), so a real "
        "publish-vs-key divergence would slip through this gate."
    )

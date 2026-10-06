"""tier_4 e2e: the bidirectional single-user mail round-trip in the real Docker image.

Stage 5 **Slice 7** of the mail-deployment VPS milestone
(tracked internally) — the closest
in-CI mirror of the milestone's actual human-acceptance scenario: **one**
provisioned user, in **one** deployed container, both *receives* a message from
an external sender (reads it back decrypted over IMAP) **and** *sends* a message
to an external recipient (DKIM-signed, relayed out). Slices 2 and 6 each proved
one direction with a single-purpose identity; this proves both directions for the
**same** identity in the same deploy — the send and receive halves of the Gmail
bar exercised together against one user/MSEK, which is what "set up a user who
sends and receives mail" actually means.

It composes the two proven round-trips onto one network + one container + one
user:
  - **Receive (Slice 2 path):** fake clamd/rspamd sidecars on the nest's network
    (the fail-closed scan gate needs reachable scanners), an inbound message
    delivered from inside the container over loopback (the DNS-dependent
    HELO/FCrDNS/sender-domain-A perimeter is loopback-exempt) but with the
    **ENFORCED DMARC perimeter** in force — the sender ``external-sender@sender.test``
    publishes a passing SPF + ``_dmarc … p=reject`` via the fake_dns sidecar, so
    acceptance is earned by an aligned auth pass — HPKE-sealed to the user's
    MSEK-derived recipient pubkey, read back **decrypted** over IMAPS(993).
  - **Send (Slice 6 path):** a stub external MX sidecar on the same network
    (reached by name via ``mta_mx_override``), an authenticated implicit-TLS
    submission on 465, DKIM-signed by the nest at the outbound hand-out,
    relayed out by the MTA, asserted to bear a
    ``DKIM-Signature`` whose ``d=`` aligns with the From: domain.

**Prod-parity posture (Gap 2g — ``docs/goal/architecture/testing.md`` § Gap 2
Target).** This canonical round-trip runs in the live-box posture, not the relaxed
opt-out: storage mode **Encrypted** (example.com's default) and the inbound AUTH
perimeter **ENFORCED** (``enforce_dmarc=true``/``log_only=false``) via the shared
``enforce_mail_perimeter`` + ``publish_passing_mail_dns`` helpers. The spam gate
stays relaxed for the synthetic loopback peer (DNSBL/FCrDNS are real-PTR/RBL gates
it can't satisfy — orthogonal to the AUTH gate). So a regression that only manifests
under Encrypted mode or an enforced DMARC perimeter — the exact class the live box
hits but a plaintext/relaxed loop hides — fails locally here. The mail seal path is
storage-mode-independent (msek-gated, not ``StorageMode``-gated), so
Encrypted exercises identical mail code while additionally proving the DKIM-signed
outbound submission works post-Encrypted-commit.

The single user is provisioned by ``provision_mail_user`` — one actor/MSEK with
the full recipient recipe (MSEK-derived recipient pubkey + wrapped-MSEK AUTH
credential + MLS snapshot) plus a wrapped submission token under the **same**
password, so the one MUA credential authenticates both the IMAP read and the SMTP
submission, exactly as a real client uses it. Because the recipient pubkey is
MSEK-derived (not a throwaway), the user's own "Sent" copy seals to a key it can
actually decrypt.

Sequencing: bring the bridges to serving (the nest already holds the primary
domain's DKIM key — nothing is provisioned for it), then exercise **receive
first** (so INBOX holds exactly the one inbound message when read) and **send
second**.
"""

import os
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
    deliver_inbound_loopback_curl,
    docker_build,
    enforce_mail_perimeter,
    find_free_port,
    find_free_ports,
    get_repo_root,
    imap_fetch_only_inbox_message,
    provision_mail_user,
    publish_passing_mail_dns,
    read_stub_mx_message,
    remove_container,
    remove_network,
    start_container_with_ports,
    start_fake_dns_sidecar,
    start_fake_scanner_sidecars,
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

DOMAIN = "localhost"               # the deployment's primary (local) mail domain
EXTERNAL_DOMAIN = "external.test"  # the relayed-to domain (mapped to the stub MX)
SENDER_DOMAIN = "sender.test"      # the inbound sender's domain (publishes passing SPF + _dmarc)
CLAIM_CODE = "BIDIR1"
USER_LOCAL = "alice"
USER_PASSWORD = "bidirectional-password-1"


# ── Fixtures ──────────────────────────────────────────────────────────


@pytest.fixture(scope="module")
def docker_image():
    """Build the image once for this module (shared tag, layer-cached)."""
    docker_build(get_repo_root())
    yield IMAGE_TAG


@pytest.fixture()
def bidi_sidecars(docker_image, tmp_path):
    """All sidecars for both directions on **one** user-defined network the nest
    joins: fake clamd + rspamd (the fail-closed inbound scan gate), a stub external
    MX (the outbound relay target), and a ``records_dir``-backed fake_dns resolver
    (the nest's ``--dns``) so the inbound sender domain can publish a PASSING SPF +
    ``_dmarc … p=reject`` for the ENFORCED DMARC perimeter (Gap 2g prod parity —
    ``testing.md`` § Gap 2 Target). The nest can't reach host listeners on this
    docker setup, so every dependency it must dial lives inside its network, reached
    by container name. Yields
    {network, clamd_addr, rspamd_url, mx_target, dns_ip, records_dir, out_dir}."""
    suffix = find_free_port()  # unique-enough token for the network + container names
    network = f"fauna-bidi-net-{suffix}"
    clamd_name = f"fauna-bidi-clamd-{suffix}"
    rspamd_name = f"fauna-bidi-rspamd-{suffix}"
    stub_name = f"fauna-bidi-stubmx-{suffix}"
    dns_name = f"fauna-bidi-dns-{suffix}"
    fakes_dir = str(get_repo_root() / "tests" / "e2e-unified" / "fakes")
    helpers_dir = str(get_repo_root() / "tests" / "e2e-unified" / "helpers")
    out_dir = tempfile.mkdtemp(prefix="fauna-bidi-stubmx-")
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
def bidi_nest(docker_image, bidi_sidecars):
    """Fresh claimed container on the sidecars' network with all four mail ports
    mapped, the scan gate pointed at the fake scanners, ``external.test`` routed to
    the stub MX (outbound), and the resolver (``--dns``) pointed at the fake_dns
    sidecar so the inbound sender domain resolves a PASSING SPF + ``_dmarc`` for the
    enforced perimeter — both directions wired into one deploy. Storage mode
    committed **Encrypted** — example.com's privacy-respecting default and the
    goal-doc prod-parity axis (``testing.md`` § Gap 2 Target); certs still provision
    via the self-signed boot bootstrap (same as the Encrypted two-nest + factory-reset
    tests), and the mail seal/store/IMAP-decrypt path is storage-mode-independent
    (msek-gated, not ``StorageMode``-gated). Cleaned up after."""
    http_port, *mail = find_free_ports(5)
    mail_ports = dict(zip((25, 465, 587, 993), mail))  # container_port -> host_port
    name = f"fauna-mail-bidi-{http_port}"
    start_container_with_ports(
        name,
        {3000: http_port, **mail_ports},
        env={
            "FAUNA_CLAIM_CODE": CLAIM_CODE,
            "FAUNA_PORT": "3000",
            # Inbound: the fail-closed scan gate dials the sidecar scanners by name.
            "FAUNA_CLAMD_ADDR": bidi_sidecars["clamd_addr"],
            "FAUNA_RSPAMD_URL": bidi_sidecars["rspamd_url"],
            # Outbound: relay external.test to the stub-MX sidecar by name (the
            # authenticated submission path is unaffected by the inbound DMARC gate).
            "FAUNA_MTA_MX_OVERRIDE": f"{EXTERNAL_DOMAIN}={bidi_sidecars['mx_target']}",
        },
        # The resolver points at the fake_dns sidecar so `verify_inbound` reads the
        # sender domain's published SPF/_dmarc for the ENFORCED DMARC gate (Gap 2g);
        # the loopback delivery is still exempt from the reachability-gated
        # HELO/FCrDNS checks, and the outbound MX is overridden (no DNS), so fake_dns
        # only answers the sender-domain SPF/DMARC TXT lookups.
        dns=bidi_sidecars["dns_ip"],
        network=bidi_sidecars["network"],
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
            "out_dir": bidi_sidecars["out_dir"],
            "records_dir": bidi_sidecars["records_dir"],
        }
    finally:
        remove_container(name)


# ── Test ──────────────────────────────────────────────────────────────


@pytest.mark.feature("mail-server")
def test_mail_deploy_bidirectional_round_trip(bidi_nest, run_seal_helper):
    nest = bidi_nest
    name = nest["name"]
    mp = nest["mail_ports"]  # container_port -> host_port
    out_dir = nest["out_dir"]
    records_dir = nest["records_dir"]

    # nest-side setup before bring-up, via the shared Gap-2g prod-parity helper:
    # register the primary mail domain (serving ordering — see the lifecycle test
    # docstring), relax ONLY the orthogonal spam gates a synthetic peer can't satisfy,
    # and ENFORCE the DMARC perimeter (`enforce_dmarc=true`, `log_only=false`). The
    # peer is loopback (`deliver_inbound_loopback_curl`), so `cross_container_peer`
    # stays False — HELO identity is loopback-exempt, but the enforced DMARC gate
    # still fires (proven by the `550` negative control in `test_mail_security_accept.py`),
    # so the perimeter is non-vacuous. Then provision the ONE bidirectional user
    # (recipient + sender on one actor/MSEK, one password for IMAP and SMTP).
    enforce_mail_perimeter(nest, domain=DOMAIN)
    user = provision_mail_user(
        nest, run_seal_helper, domain=DOMAIN, local_part=USER_LOCAL, password=USER_PASSWORD)

    # Publish the inbound sender domain's PASSING perimeter into the fake_dns: a
    # passing SPF authorizing the loopback (`spf_ip=127.0.0.1` — the loopback
    # delivery connects from inside the container, so the MTA sees 127.0.0.1 as the
    # client IP) + `_dmarc … p=reject`. Under the enforced gate above, the inbound
    # is accepted ONLY because it DMARC-passes via the aligned SPF pass (From:/envelope
    # both on `sender.test` ⇒ aspf-aligned) — the proven `test_mail_security_accept.py`
    # loopback-accept path, now exercised as the inbound leg of the round-trip.
    publish_passing_mail_dns(records_dir, {SENDER_DOMAIN: "127.0.0.1"}, spf_ip="127.0.0.1")

    bring_bridges_to_serving(name, nest, mp, DOMAIN)

    # ── RECEIVE half (first, so INBOX holds exactly the inbound message) ──
    # Deliver one unauthenticated inbound message over STARTTLS on 25, from inside
    # the container over loopback (the DNS-dependent HELO/FCrDNS/sender-domain-A
    # perimeter is loopback-exempt). The sender is `external-sender@sender.test`,
    # whose published passing SPF (ip4:127.0.0.1) + `_dmarc p=reject` clear the
    # ENFORCED DMARC gate via the aligned SPF pass — so the receive leg now runs the
    # full prod-parity AUTH perimeter, not the relaxed pass. The MTA validates the
    # recipient (alias → actor + MLS pubkey), clears the scan gate (clean via the
    # sidecar fakes), HPKE-seals the body to the user's MSEK-derived pubkey, and
    # ingests it to INBOX.
    # ASCII-only Subject: a non-ASCII char (e.g. an em-dash) would be RFC-2047
    # encoded-word'd in the header, so a literal-substring assert would miss.
    in_subject = "Stage-5 bidirectional inbound"
    in_marker = "deploy-image bidirectional inbound"
    in_msg_id = deliver_inbound_loopback_curl(
        name,
        mail_from=f"external-sender@{SENDER_DOMAIN}",
        rcpt_to=user["username"],
        subject=in_subject,
        body_text=f"Hello {user['username']} from the {in_marker} test.\n",
    )

    # Read it back, decrypted: the MDA AEAD-unwraps the wrapped MSEK under the
    # password at AUTH, Decrypts the snapshot, and OpenMailRecord-opens the body —
    # same MSEK on both ends ⇒ the open succeeds and the client sees plaintext.
    try:
        raw = imap_fetch_only_inbox_message(
            "127.0.0.1", mp[993], DOMAIN, user["username"], user["password"],
        )
    except (AssertionError, TimeoutError) as e:
        raise AssertionError(
            f"inbound read-back failed: {e}\n\n"
            f"── bridge diagnostics ──\n{bridge_diag(name)}") from e

    text = raw.decode("utf-8", errors="replace")
    assert in_subject in text, (
        f"decrypted inbound body must carry the Subject; got first 400B: {text[:400]!r}")
    assert in_marker in text, (
        f"decrypted inbound body must carry the message text; got first 400B: {text[:400]!r}")
    assert in_msg_id.strip("<>") in text, "decrypted inbound body must carry the Message-ID"

    # ── SEND half (same user submits to an external recipient) ──
    token = f"bidi-{int(time.time() * 1000)}"
    rcpt = f"recipient@{EXTERNAL_DOMAIN}"
    raw_message = (
        f"From: Alice <{user['username']}>\r\n"
        f"To: {rcpt}\r\n"
        f"Subject: Stage-5 bidirectional outbound {token}\r\n"
        f"Message-ID: <{token}@{DOMAIN}>\r\n"
        "Date: Sun, 25 May 2026 12:30:00 +0000\r\n"
        "MIME-Version: 1.0\r\n"
        "Content-Type: text/plain; charset=utf-8\r\n"
        "\r\n"
        "Hello from the deploy-image bidirectional outbound test.\r\n"
    ).encode()

    try:
        submit_message_tls(
            "127.0.0.1", mp[465], DOMAIN,
            sender=user["username"], password=user["password"], rcpt=rcpt,
            raw_message=raw_message,
        )
    except (AssertionError, OSError, ssl.SSLError) as e:
        raise AssertionError(
            f"submission failed: {e}\n\n── bridge diagnostics ──\n{bridge_diag(name)}") from e

    # Relay out, signed: the nest signs the message at the hand-out and the
    # outbound worker drains it immediately (submission triggers a poll) to the
    # stub external MX.
    received = read_stub_mx_message(out_dir, token, timeout=30.0)
    assert received is not None, (
        f"stub external MX received no message tagged {token} within 30s — outbound "
        f"delivery did not complete.\n\n── bridge diagnostics ──\n{bridge_diag(name)}")
    assert b"dkim-signature:" in received.lower(), (
        f"delivered message has no DKIM-Signature header — the nest handed out "
        f"unsigned mail. First 600 bytes:\n{received[:600]!r}")
    # DMARC alignment proxy: the signature's d= equals the From: domain.
    d_tag = dkim_signature_tag(received, "d")
    assert d_tag == DOMAIN, (
        f"DKIM-Signature d= must align with the From: domain {DOMAIN!r} "
        f"(Gmail dkim=pass + DMARC-aligned proxy); got d={d_tag!r}")

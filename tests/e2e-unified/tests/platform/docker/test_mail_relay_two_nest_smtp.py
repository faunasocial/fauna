"""tier_4 e2e: a true two-party EXTERNAL SMTP round-trip A→B→reply→A in the real
Docker image — **Gap 2c + 2g** (tracked internally).

This is the **prod-parity default tier_4 posture** (``docs/goal/architecture/testing.md``
§ Gap 2 Target): the canonical two-party loop (nest-A ⇄ nest-B over the
``fakes/fake_dns.py`` sidecar + ``docker run --dns``) with the inbound **AUTH
perimeter ENFORCED** — ``enforce_dmarc=true``/``log_only=false`` on both boxes, each
publishing ``_dmarc … p=reject``, so a delivered message is accepted ONLY if it
DMARC-passes (which the cross-container leg does via the aligned SPF pass). Gap 2g
is "make the full-perimeter prod-parity round-trip the *default*, not an opt-in
subset"; this converts the formerly-relaxed 2c loop into that default. The spam
gate stays relaxed for the synthetic docker peer (dnsbl/fcrdns/HELO-identity are
real-PTR/RBL gates a container peer can't satisfy — orthogonal to the AUTH gate
under enforcement). No-modes retirement (ratified 2026-07-12): both boxes used
to additionally commit **Encrypted** storage mode (slice 2, 2026-06-06) to
mirror example.com's default; that commit is gone along with the axis itself —
every nest is sealed at rest unconditionally now, so this loop is still the
closest in-CI mirror of the live box (enforced AUTH perimeter +
cross-container real-DNS SMTP) with nothing left to additionally commit. The
mail seal path was always storage-mode-independent (msek-gated, not
``StorageMode``-gated — § 2d), so this test still exercises identical mail
code, including the direct ``provision_mail_user`` recipe (the one path the
client-``enable_mail``-based factory-reset test never covers). **Remaining 2g axis
(deferred, lower value / higher uncertainty):**
greylist-on-cross-container (depends on the outbound worker's retry scheduler; the
loopback greylist-retry is already covered by 2a).

The existing two-box test (``test_mail_relay_two_nest.py``) moves mail box→box
over the **federation** channel (the home box *pulls* the sealed record from the
public box), so the SMTP inbound perimeter runs exactly **once** (only the public
box receives over SMTP). This test instead delivers over **real MTA→MTA SMTP** in
*both* directions, so the inbound receive perimeter (sender-domain DNS check,
recipient resolution, scan gate, HPKE seal, ingest) is exercised on **both legs**:

  1. **Forward** — ``alice@alpha.test`` (on box A) submits to ``bob@beta.test``.
     Box A's outbound worker resolves ``beta.test`` over the **production**
     ``LiveMXResolver`` path (a genuine DNS MX→A lookup, *not* the
     ``mta_mx_override`` operator-hatch) and delivers to box B's MTA on :25. Box
     B's inbound perimeter accepts it from a non-loopback peer, seals it to bob,
     and bob reads it back **decrypted** over IMAPS.
  2. **Reply** — ``bob@beta.test`` (on box B) replies to ``alice@alpha.test``;
     box B relays to box A's MTA over SMTP the same way, and alice reads the
     decrypted reply over IMAPS.

What only this tier catches: the cross-container SMTP receive path — every
existing inbound test delivers from *inside* the container over loopback
(``deliver_inbound_loopback_curl``), which is exempt from the DNS-dependent
HELO/FCrDNS/sender-domain gates and never drives a real fauna *outbound* worker
into a real fauna *inbound* MX. Here a real MTA dials a real MTA across the docker
network, exercising EHLO → MAIL → RCPT → STARTTLS-opportunistic → DATA end to
end, on the packaged binaries under s6 supervision.

**Production fidelity / configuration model.** Every *nest configuration* here is
set through the client/admin WS-RPC path the Fauna app UI uses
(``register_primary_domain`` → ``fauna.bridges.add_local_domain``,
``put_spam_policy``, alias/MLS provisioning) — never an
env var. The env vars are deployment
*topology* the compose file fixes at deploy (``FAUNA_PORT``/
``FAUNA_CLAIM_CODE``, the scanner-sidecar addresses) and the docker ``--dns`` flag
points each container's resolver at a ``fake_dns`` sidecar standing in for public
DNS — exactly as the SPF/DMARC perimeter tests already do. Crucially this test
uses **no** ``FAUNA_MTA_MX_OVERRIDE``: A→B and B→A route over real DNS, so the
outbound path under test is the production one.

**Topology / the boot-time chicken-and-egg.** The NODE domain (set by the admin
claim on the domainless image, *not* an env var) is ``localhost`` on both boxes —
the ACME-off cold boot the deploy image special-
cases (a non-localhost node domain triggers ACME issuance at boot, which can't
reach Let's Encrypt in this hermetic network → the nest never goes healthy). The
PRIMARY MAIL domain is each box's external domain (``alpha.test`` on A,
``beta.test`` on B), registered via ``register_primary_domain``: the *mail primary
domain*, not the node domain, is the outbound EHLO host AND the submission's
canonical envelope sender (``submission.go``) AND the mail-listener cert CN — so a
``localhost`` mail primary would make A announce HELO ``localhost`` (the receiver
rejects ``554`` from a non-loopback peer) and rebind the sender to
``alice@localhost``. Each box's outbound resolver must map the
*other* box's mail domain to the *other* box's container IP, which is only known
after both containers start — so the ``fake_dns`` sidecar is ``records_dir``-
backed: the boxes point at it via ``--dns`` at boot, and once both are up the host
publishes ``{alpha.test→A_ip, beta.test→B_ip}`` A + MX + passing SPF records into
its records file (``write_dns_records``). HELO *identity* is dropped on the
receivers (``relax_spam_policy(helo_identity_required=False)``) — it would also
pass via the published A record, but v1 keeps one fewer live-lookup dependency;
the sender-domain MX/A gate and SPF still run cross-container. **The AUTH gate is
ENFORCED** (``put_auth_policy(enforce_dmarc=true, log_only=false)`` + the published
``_dmarc … p=reject``) — that is the 2g prod-parity upgrade over the formerly-relaxed
loop. The shared fail-closed ``fake_clamd``/``fake_rspamd`` sidecars satisfy the C.7
scan gate on both boxes. Storage committed **Encrypted** (example.com's default and
the goal-doc prod-parity axis — `testing.md` § Gap 2 Target; certs still provision
via the self-signed boot bootstrap, same as the Encrypted factory-reset test); the
mail seal path is storage-mode-independent, so this exercises
identical mail code and additionally proves the direct `provision_mail_user` recipe
works post-Encrypted-commit.

Spec: ``docs/goal/behavior/smtp-server.md`` (§ Sender-domain, § Inbound
perimeter, § Outbound MX resolution) + ``imap-server.md`` § Read-back. tier_4
rationale: real MTA→MTA over the packaged image,
the seam binary/loopback tests bypass.
"""

import subprocess
import time

import pytest


from .helpers import (
    IMAGE_TAG,
    bridge_diag,
    bring_bridges_to_serving,
    claim_admin_api,
    container_ip,
    create_network,
    docker_build,
    enforce_mail_perimeter,
    find_free_port,
    find_free_ports,
    get_repo_root,
    imap_fetch_only_inbox_message,
    provision_mail_user,
    publish_passing_mail_dns,
    remove_container,
    remove_network,
    start_container_with_ports,
    start_fake_dns_sidecar,
    start_fake_scanner_sidecars,
    submit_message_tls,
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

# Node domain vs mail domain (a deliberate split):
#   * NODE domain (set by the admin claim, not an env var) = `localhost` on both
#     boxes — the ACME-OFF cold boot the deploy image special-cases; a
#     non-localhost node domain triggers
#     ACME issuance at boot, which can't reach Let's Encrypt in this hermetic
#     network and leaves the nest unhealthy (health timeout).
#   * PRIMARY MAIL domain = the box's EXTERNAL domain (`alpha.test`/`beta.test`),
#     registered via `register_primary_domain`. The mail primary domain — NOT the
#     node domain — is the outbound EHLO host and the submission's canonical
#     envelope sender (`submission.go::authedFromAddress` + the EHLO anchor) and
#     the mail listeners' cert CN, so it must be a real public-style domain: a
#     `localhost` mail primary makes A announce HELO `localhost` (the receiver
#     rejects `554` from a non-loopback peer) and rebinds alice to `alice@localhost`.
# `.test` is a reserved TLD, so each external domain is its own registrable domain.
DOMAIN_A = "alpha.test"
DOMAIN_B = "beta.test"
ALICE_LOCAL = "alice"
BOB_LOCAL = "bob"
ALICE_PW = "two-nest-smtp-alice-pw-1"
BOB_PW = "two-nest-smtp-bob-pw-1"
CLAIM_A = "RLYA2C"
CLAIM_B = "RLYB2C"


# ── Fixtures ──────────────────────────────────────────────────────────


@pytest.fixture(scope="module")
def docker_image():
    """Build the image once for this module (shared tag, layer-cached)."""
    docker_build(get_repo_root())
    yield IMAGE_TAG


@pytest.fixture()
def smtp_relay_net(docker_image, tmp_path):
    """A fresh user-defined network carrying a `records_dir`-backed fake_dns
    resolver (both boxes' `--dns`) and the shared fail-closed scan sidecars.
    Yields {network, dns_ip, records_dir, clamd_addr, rspamd_url}."""
    suffix = find_free_port()  # unique-enough token for the names
    network = f"fauna-2csmtp-net-{suffix}"
    dns_name = f"fauna-2csmtp-dns-{suffix}"
    clamd_name = f"fauna-2csmtp-clamd-{suffix}"
    rspamd_name = f"fauna-2csmtp-rspamd-{suffix}"
    fakes_dir = str(get_repo_root() / "tests" / "e2e-unified" / "fakes")
    records_dir = str(tmp_path)
    import os as _os
    _os.chmod(records_dir, 0o777)  # the sidecar reads it through a bind mount
    create_network(network)
    try:
        scanners = start_fake_scanner_sidecars(
            network, fakes_dir, clamd_name=clamd_name, rspamd_name=rspamd_name)
        dns_ip = start_fake_dns_sidecar(
            network, fakes_dir, name=dns_name, txt_records={},
            records_dir=records_dir)
        yield {"network": network, "dns_ip": dns_ip, "records_dir": records_dir,
               **scanners}
    finally:
        remove_container(dns_name)
        remove_container(clamd_name)
        remove_container(rspamd_name)
        remove_network(network)


def _start_box(net, dns_ip, scanners, *, name, domain, claim_code, http_port, mail_ports):
    """Start one claimed full-MTA+MDA box on `net`, resolver pointed at the fake
    DNS, scan gate at the shared sidecars. The NODE domain is `localhost` (ACME-off
    boot); `domain` is the box's primary MAIL domain, registered later. Storage
    committed **Encrypted** — example.com's privacy-respecting default (the
    storage-mode redesign) and the goal-doc prod-parity posture (`testing.md` §
    Gap 2 Target). The mail seal/store/IMAP-decrypt path is storage-mode-independent
    (msek-gated, not `StorageMode`-gated), so this exercises identical
    mail code to the former plaintext box AND proves the direct `provision_mail_user`
    recipe (a fresh client-minted per-recipient MSEK + sealed-blob uploads, distinct
    from the client `enable_mail` path the factory-reset test uses) works post-
    Encrypted-commit. Returns the box dict (incl. its learned container IP, for the
    DNS A/SPF records published once both boxes are up)."""
    start_container_with_ports(
        name,
        {3000: http_port, **mail_ports},
        env={
            "FAUNA_CLAIM_CODE": claim_code,
            "FAUNA_PORT": "3000",
            "FAUNA_CLAMD_ADDR": scanners["clamd_addr"],
            "FAUNA_RSPAMD_URL": scanners["rspamd_url"],
        },
        network=net,
        dns=dns_ip,
    )
    wait_for_health(http_port, name)
    admin = claim_admin_api(http_port, claim_code, handle="admin")
    nest = {
        "name": name, "port": http_port, "domain": domain,
        "url": f"https://127.0.0.1:{http_port}", "admin": admin,
        "mail_ports": mail_ports, "ip": container_ip(name),
    }
    return nest


@pytest.fixture()
def two_boxes(docker_image, smtp_relay_net):
    """Box A (serves alpha.test) and box B (serves beta.test), both on the relay
    net with all four mail ports mapped. Yields {a, b, records_dir}."""
    net = smtp_relay_net["network"]
    dns_ip = smtp_relay_net["dns_ip"]
    a_http, a25, a465, a587, a993 = find_free_ports(5)
    b_http, b25, b465, b587, b993 = find_free_ports(5)
    a_name = f"fauna-2csmtp-a-{a_http}"
    b_name = f"fauna-2csmtp-b-{b_http}"
    try:
        a = _start_box(net, dns_ip, smtp_relay_net, name=a_name, domain=DOMAIN_A,
                       claim_code=CLAIM_A, http_port=a_http,
                       mail_ports={25: a25, 465: a465, 587: a587, 993: a993})
        b = _start_box(net, dns_ip, smtp_relay_net, name=b_name, domain=DOMAIN_B,
                       claim_code=CLAIM_B, http_port=b_http,
                       mail_ports={25: b25, 465: b465, 587: b587, 993: b993})
        yield {"a": a, "b": b, "records_dir": smtp_relay_net["records_dir"]}
    finally:
        remove_container(a_name)
        remove_container(b_name)


# ── Test ────────────────────────────────────────────────────────────────


@pytest.mark.feature("mail-server")
def test_two_party_external_smtp_round_trip_and_reply(two_boxes, run_seal_helper):
    a = two_boxes["a"]
    b = two_boxes["b"]
    records_dir = two_boxes["records_dir"]
    a_mp = a["mail_ports"]
    b_mp = b["mail_ports"]

    # ── nest-side setup (before bring-up), via the client/admin RPC path: each box
    # enters the prod-parity inbound posture (`enforce_mail_perimeter`, the shared
    # Gap-2g helper) — register its external domain as its primary MAIL domain (the
    # mail cert anchor == outbound EHLO host == submission sender domain, distinct
    # from the node domain `localhost`), relax ONLY the orthogonal spam gates the
    # synthetic docker peer can't satisfy (dnsbl/fcrdns/conn-rate), and ENFORCE the
    # AUTH gate (`enforce_dmarc=true`, `log_only=false`) — the perimeter-PASSING half
    # of prod parity, not relaxed-everything. `cross_container_peer=True` drops HELO
    # identity: the peer is a non-loopback container IP whose EHLO host (the mail
    # primary) can't A-resolve to it (HELO would also pass via the published A record,
    # but we keep one fewer live-lookup dependency for v1). Each box publishes
    # `_dmarc … p=reject` (below), so a delivered message clears the enforced gate
    # (`mta/auth_enforce.go::applyDMARCRejectGate`) ONLY if it DMARC-passes — which
    # the cross-container leg does via the aligned SPF pass (the proven
    # SPF→DMARC-aspf path of `test_mail_security_accept.py`). One mail user per box.
    for box in (a, b):
        enforce_mail_perimeter(box, domain=box["domain"], cross_container_peer=True)
    alice = provision_mail_user(a, run_seal_helper, domain=DOMAIN_A,
                                local_part=ALICE_LOCAL, password=ALICE_PW)
    bob = provision_mail_user(b, run_seal_helper, domain=DOMAIN_B,
                              local_part=BOB_LOCAL, password=BOB_PW)

    bring_bridges_to_serving(a["name"], a, a_mp, DOMAIN_A)
    bring_bridges_to_serving(b["name"], b, b_mp, DOMAIN_B)

    # ── Publish DNS now that both container IPs are known (the boot chicken-and-
    # egg): `publish_passing_mail_dns` (the shared Gap-2g helper) writes, for each
    # external domain → the box that serves it, an A record, a self-MX (`0 <domain>`
    # so `LookupMX` succeeds before the A-record dial), a passing SPF authorizing
    # that box's real container IP (default `spf_ip` = each domain's own IP — the
    # cross-container leg, where SPF must pass against the connecting container IP),
    # and a `_dmarc … p=reject` policy. The `p=reject` makes the enforced DMARC gate
    # (above) non-vacuous: a message is accepted ONLY if DMARC passes, which it does
    # via the aligned SPF pass (From:/envelope both on the sender domain →
    # aspf-aligned). This is the prod-parity perimeter-PASSING round-trip (Gap 2g):
    # legit aligned mail accepted *because it authenticated*, on a real MTA→MTA leg.
    publish_passing_mail_dns(records_dir, {DOMAIN_A: a["ip"], DOMAIN_B: b["ip"]})

    alice_addr = f"{ALICE_LOCAL}@{DOMAIN_A}"
    bob_addr = f"{BOB_LOCAL}@{DOMAIN_B}"
    diag = lambda box: f"\n\n── bridge diag {box['name']} ──\n{bridge_diag(box['name'])}"  # noqa: E731

    # ── Forward leg: alice (box A) → bob@beta.test. A authenticates the
    # submission, binds From to alice, then its outbound worker resolves beta.test
    # over real DNS (MX → A) and relays to box B's MTA :25 across the network.
    fwd_token = f"fwd-{int(time.time() * 1000)}"
    fwd_msgid = f"<{fwd_token}@{DOMAIN_A}>"
    fwd_subject = f"2c forward external SMTP {fwd_token}"
    fwd_body = "Hello bob, this is the forward leg over real MTA-to-MTA SMTP.\r\n"
    fwd_message = (
        f"From: Alice <{alice_addr}>\r\n"
        f"To: {bob_addr}\r\n"
        f"Subject: {fwd_subject}\r\n"
        f"Message-ID: {fwd_msgid}\r\n"
        "Date: Mon, 01 Jun 2026 09:00:00 +0000\r\n"
        "MIME-Version: 1.0\r\n"
        "Content-Type: text/plain; charset=utf-8\r\n"
        "\r\n"
        f"{fwd_body}"
    ).encode()
    try:
        submit_message_tls("127.0.0.1", a_mp[465], DOMAIN_A, sender=alice_addr,
                           password=ALICE_PW, rcpt=bob_addr, raw_message=fwd_message)
    except Exception as e:
        raise AssertionError(f"alice submission failed: {e}{diag(a)}") from e

    # Settle before polling: `imap_fetch_only_inbox_message` reconnects + re-AUTHs
    # every 1s, and each AUTH does a `fetch_wrapped_mls_blob` rate-limited to
    # 30 events / 60s per (bridge, actor, credential) (`bridge_rate_limit.rs`). A
    # cross-container relay is slower to land than a loopback delivery, so a brief
    # head start keeps the poll from exhausting that budget on an empty INBOX
    # before the mail arrives (the bounce-driven failure mode of the first run).
    time.sleep(8)

    # bob reads the decrypted forward over IMAPS on box B (the receive perimeter
    # ran on a non-loopback peer: sender-domain check on alpha.test, recipient
    # resolution, scan gate, seal-to-bob, ingest).
    try:
        raw = imap_fetch_only_inbox_message("127.0.0.1", b_mp[993], DOMAIN_B,
                                            bob["username"], bob["password"], timeout=120.0)
    except (AssertionError, TimeoutError) as e:
        raise AssertionError(f"{e}{diag(b)}{diag(a)}") from e
    text = raw.decode("utf-8", errors="replace")
    assert fwd_subject in text, f"box B must serve the decrypted forward Subject; first 400B: {text[:400]!r}"
    assert "forward leg over real MTA-to-MTA SMTP" in text, (
        f"box B must serve the decrypted forward body; first 400B: {text[:400]!r}")
    assert fwd_token in text, "box B must serve the decrypted forward Message-ID"

    # ── Reply leg: bob (box B) → alice@alpha.test. Symmetric — B relays to A's MTA
    # :25, A's inbound perimeter runs (sender-domain check on beta.test), seals to
    # alice, and alice reads the reply. This is the leg the federation-pull two-box
    # test never exercises over SMTP.
    rep_token = f"rep-{int(time.time() * 1000)}"
    rep_subject = f"Re: {fwd_subject}"
    rep_message = (
        f"From: Bob <{bob_addr}>\r\n"
        f"To: {alice_addr}\r\n"
        f"Subject: {rep_subject}\r\n"
        f"Message-ID: <{rep_token}@{DOMAIN_B}>\r\n"
        f"In-Reply-To: {fwd_msgid}\r\n"
        "Date: Mon, 01 Jun 2026 09:05:00 +0000\r\n"
        "MIME-Version: 1.0\r\n"
        "Content-Type: text/plain; charset=utf-8\r\n"
        "\r\n"
        "Hi alice, replying back over the second SMTP leg.\r\n"
    ).encode()
    try:
        submit_message_tls("127.0.0.1", b_mp[465], DOMAIN_B, sender=bob_addr,
                           password=BOB_PW, rcpt=alice_addr, raw_message=rep_message)
    except Exception as e:
        raise AssertionError(f"bob reply submission failed: {e}{diag(b)}") from e

    time.sleep(8)  # same settle as the forward leg (blob-fetch rate-limit budget)
    try:
        raw = imap_fetch_only_inbox_message("127.0.0.1", a_mp[993], DOMAIN_A,
                                            alice["username"], alice["password"], timeout=120.0)
    except (AssertionError, TimeoutError) as e:
        raise AssertionError(f"{e}{diag(a)}{diag(b)}") from e
    text = raw.decode("utf-8", errors="replace")
    assert rep_subject in text, f"box A must serve the decrypted reply Subject; first 400B: {text[:400]!r}"
    assert "replying back over the second SMTP leg" in text, (
        f"box A must serve the decrypted reply body; first 400B: {text[:400]!r}")
    assert rep_token in text, "box A must serve the decrypted reply Message-ID"

"""tier_4 e2e: the MTA security perimeter ACCEPTS legitimate, authenticated mail.

The positive control to ``test_mail_security_relay.py`` (which proves the
perimeter *rejects* open relay, unauthenticated submission, and local-domain
spoofing). A perimeter's contract is two-sided: it must reject abuse AND deliver
legitimate aligned mail. Every tier_4 perimeter assert to date is rejection-only;
this closes Gap 2b (tracked internally) — a legit
inbound that PASSES SPF + DMARC alignment is accepted and readable, proven
against the real Docker image with DMARC enforcement ON.

What it asserts, against the real image with the bridges brought to *serving* and
``enforce_dmarc=true`` (``log_only=false``):

  1. **Aligned-pass accept (positive).** An inbound from an external domain that
     publishes a *passing* SPF record (``v=spf1 ip4:127.0.0.1 -all`` — the
     loopback delivery's client IP) plus a ``_dmarc … p=reject`` policy, with the
     ``From:`` and envelope sender both on that domain (DMARC aspf-aligned), is
     ACCEPTED (250 on DATA) and read back **decrypted** over IMAPS. Acceptance is
     non-trivial: under an *enforced* ``p=reject`` the message can only clear the
     auth-enforce gate (``mta/auth_enforce.go::applyDMARCRejectGate``) if DMARC
     PASSED — and with no DKIM signature, DMARC passes only via the aligned SPF
     pass. So a green read proves the full SPF -> DMARC-alignment -> accept path.

  2. **Fail-still-rejected (negative control, SAME fixture).** A second inbound
     from a domain publishing ``v=spf1 -all`` (hard fail) under the same enforced
     ``p=reject`` is REJECTED ``550 5.7.1 DMARC reject`` at DATA. This proves the
     enforcement is genuinely live in *this* fixture — so the positive's
     acceptance reflects a real PASS, not silently-disabled enforcement.

Why SPF over a loopback delivery passes: inbound is delivered from inside the
container over loopback (``deliver_inbound_loopback_curl``), so the DNS-dependent
HELO/FCrDNS perimeter is loopback-exempt and the sender-domain MX/A check is
skipped (``server.go``: ``SkipSenderDomainLoopback || !isLoopbackIP`` — off for
loopback by default, so a sender domain with no A/MX record is fine — which is
also why ``fake_dns`` needs no MX support here). But ``verify_inbound``
(``libs/fauna-mail/src/auth.rs``, phase C.5) still runs SPF against the real
connecting IP (``127.0.0.1``) and DMARC against the published ``_dmarc`` policy —
both resolved through a ``fakes/fake_dns`` sidecar the container's resolver points
at (``--dns``). So ``ip4:127.0.0.1`` matches the loopback client IP -> SPF Pass ->
DMARC aspf-aligned Pass.

Topology: one user-defined network carrying the DNS sidecar (the resolver) AND
the fail-closed ``fakes/{fake_clamd,fake_rspamd}`` scan sidecars — a PASSING
message reaches the C.7 scan gate after clearing auth-enforce (unlike the
rejection tests, which never reach DATA / are rejected before the scan gate, so
they need no scanners). Storage committed plaintext (the deploy default → the
self-signed bootstrap cert provisions). Cleaned up after.

Spec: ``docs/goal/behavior/smtp-server.md`` § Error / tempfail strategy (the
``DMARC reject`` row) + § Inbound auth-enforcement; ``imap-server.md`` §
Read-back. tier_4 rationale: the deployed image's
auth-enforce gate + cold-boot config-fetch, not the isolated Go unit coverage.

Inbound *DKIM verification* (receive an externally-DKIM-signed message, assert it
passes) is the second test here (``test_perimeter_accepts_dkim_aligned_pass_rejects_broken``):
a third sender domain (``DKIM_DOMAIN``) publishes ``v=spf1 -all`` so SPF can never
align, plus a ``<selector>._domainkey`` RSA pubkey. dkimpy (the independent
external signer — never our own ``fauna_mail`` signer, so it's a genuine interop
proof of the ``mail-auth`` verifier) signs a message with the matching key; under
the enforced ``p=reject`` the message clears the gate ONLY via the DKIM-aligned
pass. A body-tampered twin (broken ``bh=``) is rejected ``550 5.7.1 DMARC reject``,
proving the verifier checks the signature cryptographically. This closes Gap-2b in
full.
"""

import base64
import email.message
import email.utils
import subprocess
import uuid

import dkim  # dkimpy — independent external signer; fleet venv dep (never our mail-auth)
import pytest
from cryptography.hazmat.primitives import serialization
from cryptography.hazmat.primitives.asymmetric import rsa


from .helpers import (
    IMAGE_TAG,
    bridge_diag,
    bring_bridges_to_serving,
    claim_admin_api,
    create_network,
    deliver_inbound_attempt_loopback_curl,
    deliver_inbound_loopback_curl,
    deliver_inbound_raw_loopback_curl,
    docker_build,
    find_free_port,
    find_free_ports,
    get_repo_root,
    imap_fetch_only_inbox_message,
    provision_mail_recipient,
    put_auth_policy,
    register_primary_domain,
    relax_spam_policy,
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

DOMAIN = "localhost"
CLAIM_CODE = "ACCPT1"
RECIPIENT_LOCAL = "inbox-user"
RECIPIENT_PASSWORD = "perimeter-pass-password-1"

# Two external sender domains the fake-DNS sidecar publishes policy for. `.test`
# is a reserved TLD (RFC 6761) the public-suffix list treats as a normal TLD, so
# each is its own registrable domain and `_dmarc.<domain>` is the exact policy
# location (no org-domain fallback). Neither is a *local* domain — these are
# legitimate external senders, the inverse of the spoof test's local-domain spoof.
PASS_DOMAIN = "sender-ok.test"     # SPF passes against the loopback client IP
FAIL_DOMAIN = "sender-bad.test"    # SPF hard-fails (`-all`) — negative control

# A third external sender domain whose ONLY passing aligned identifier is DKIM:
# it publishes `v=spf1 -all` (SPF hard-fails for every client IP, so SPF can
# never align) + `p=reject` + a `<selector>._domainkey` RSA pubkey. So an inbound
# from this domain clears the enforced `p=reject` gate iff its DKIM signature
# verifies and aligns — isolating the DKIM-only DMARC-pass path (the 2b SPF half
# isolated the SPF-only path). RFC-6761 `.test` is its own registrable domain.
DKIM_DOMAIN = "sender-dkim.test"
DKIM_SELECTOR = "e2e"


# ── Fixtures ──────────────────────────────────────────────────────────


@pytest.fixture(scope="module")
def docker_image():
    """Build the image once for this module (shared tag, layer-cached)."""
    docker_build(get_repo_root())
    yield IMAGE_TAG


@pytest.fixture(scope="module")
def dkim_keypair():
    """A fresh RSA-2048 DKIM keypair for the DKIM-aligned-pass test, generated
    once per module. Returns the PKCS8 private-key PEM (for ``dkim.sign``) and the
    base64 SubjectPublicKeyInfo DER (the ``p=`` value of the published DKIM TXT
    record). RSA over ed25519 because dkimpy's RSA path is dependency-free here and
    our verifier (``mail-auth``) accepts ``k=rsa`` + ``k=ed25519`` alike; the
    independent signer (dkimpy, NOT our ``fauna_mail`` signer) makes this a genuine
    interop proof of the verifier, not a circular sign-then-verify."""
    priv = rsa.generate_private_key(public_exponent=65537, key_size=2048)
    priv_pem = priv.private_bytes(
        serialization.Encoding.PEM,
        serialization.PrivateFormat.PKCS8,
        serialization.NoEncryption(),
    )
    pub_der = priv.public_key().public_bytes(
        serialization.Encoding.DER,
        serialization.PublicFormat.SubjectPublicKeyInfo,
    )
    return {"priv_pem": priv_pem, "pub_b64": base64.b64encode(pub_der).decode()}


@pytest.fixture()
def accepting_nest(docker_image, dkim_keypair):
    """A fresh claimed container on a user-defined network carrying BOTH a
    ``fake_dns`` resolver (publishing passing/failing SPF + ``p=reject`` DMARC for
    the two external sender domains) AND the fail-closed ``fake_clamd``/``fake_rspamd``
    scan sidecars (a PASSING inbound reaches the C.7 scan gate). The container's
    resolver points at the DNS sidecar (``--dns``) so ``verify_inbound``'s
    ``new_system_conf()`` reads the published policy; the scan gate dials the
    scanners by IP literal (DNS-independent). Storage committed plaintext (deploy
    default → self-signed cert provisions). Cleaned up after."""
    suffix = find_free_port()  # unique token for the network + sidecar names
    network = f"fauna-accept-net-{suffix}"
    dns_name = f"fauna-accept-dns-{suffix}"
    clamd_name = f"fauna-accept-clamd-{suffix}"
    rspamd_name = f"fauna-accept-rspamd-{suffix}"
    fakes_dir = str(get_repo_root() / "tests" / "e2e-unified" / "fakes")
    http_port, *mail = find_free_ports(5)
    mail_ports = dict(zip((25, 465, 587, 993), mail))  # container_port -> host_port
    name = f"fauna-mail-accept-{http_port}"
    create_network(network)
    try:
        scanners = start_fake_scanner_sidecars(
            network, fakes_dir, clamd_name=clamd_name, rspamd_name=rspamd_name)
        dns_ip = start_fake_dns_sidecar(
            network, fakes_dir, name=dns_name,
            txt_records={
                # PASS: authorize the loopback client IP so SPF Pass; aligned
                # `From:`/envelope on this domain → DMARC aspf-pass despite p=reject.
                PASS_DOMAIN: "v=spf1 ip4:127.0.0.1 -all",
                f"_dmarc.{PASS_DOMAIN}": "v=DMARC1; p=reject",
                # FAIL: no authorized hosts → SPF hard-fail for any client IP; no
                # DKIM → DMARC has no passing aligned identifier → p=reject fires.
                FAIL_DOMAIN: "v=spf1 -all",
                f"_dmarc.{FAIL_DOMAIN}": "v=DMARC1; p=reject",
                # DKIM: SPF hard-fails (so SPF can never align) but the selector
                # publishes an RSA pubkey, so a validly-signed+aligned message
                # passes DMARC via DKIM alone. The pubkey is this module's freshly
                # generated keypair (`dkim_keypair`); the test signs with its
                # private half.
                DKIM_DOMAIN: "v=spf1 -all",
                f"_dmarc.{DKIM_DOMAIN}": "v=DMARC1; p=reject",
                f"{DKIM_SELECTOR}._domainkey.{DKIM_DOMAIN}":
                    f"v=DKIM1; k=rsa; p={dkim_keypair['pub_b64']}",
            },
        )
        start_container_with_ports(
            name,
            {3000: http_port, **mail_ports},
            env={
                "FAUNA_CLAIM_CODE": CLAIM_CODE,
                "FAUNA_PORT": "3000",
                # Fail-closed scan gate → operator-hatch at the sidecar scanners,
                # reachable by IP literal on the shared network.
                "FAUNA_CLAMD_ADDR": scanners["clamd_addr"],
                "FAUNA_RSPAMD_URL": scanners["rspamd_url"],
            },
            network=network,
            dns=dns_ip,
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
            yield nest
        finally:
            remove_container(name)
    finally:
        remove_container(dns_name)
        remove_container(clamd_name)
        remove_container(rspamd_name)
        remove_network(network)


# ── Test ────────────────────────────────────────────────────────────────


@pytest.mark.feature("mail-abuse-refused")
def test_perimeter_accepts_aligned_pass_rejects_fail(accepting_nest, run_seal_helper):
    """Positive + negative control in one fixture: an SPF-aligned-pass inbound is
    accepted (250 + IMAP read) under an enforced ``p=reject``, while a ``-all``
    sender under the same policy is rejected ``550 5.7.1 DMARC reject``."""
    nest = accepting_nest
    name = nest["name"]
    mp = nest["mail_ports"]  # container_port -> host_port

    # Setup targets nest (not the bridge), so before enable: primary domain
    # (serving ordering), spam perimeter relaxed (loopback peer accepted — this is
    # the SPAM gate, independent of the AUTH gate under test), DMARC enforcement ON
    # (so the verdict gates delivery), and a valid local recipient.
    register_primary_domain(nest, DOMAIN)
    relax_spam_policy(nest)
    put_auth_policy(nest, enforce_dmarc=True, log_only=False)
    recipient = provision_mail_recipient(
        nest, run_seal_helper, domain=DOMAIN, local_part=RECIPIENT_LOCAL,
        password=RECIPIENT_PASSWORD,
    )

    bring_bridges_to_serving(name, nest, mp, DOMAIN)

    diag = lambda: f"\n\n── bridge diagnostics ──\n{bridge_diag(name)}"  # noqa: E731

    # (1, positive) A legit inbound from PASS_DOMAIN: SPF `ip4:127.0.0.1` matches
    # the loopback client IP → Pass; `From:`/envelope both on PASS_DOMAIN → DMARC
    # aspf-aligned Pass; so it clears the enforced `p=reject` auth-enforce gate and
    # is ingested. `deliver_inbound_loopback_curl` raises on a non-250 (a reject
    # would mean the pass path failed) — append bridge_diag so that self-diagnoses.
    subject = "perimeter-pass aligned accept"
    body_marker = "deploy-image perimeter PASS round-trip"
    try:
        msg_id = deliver_inbound_loopback_curl(
            name,
            mail_from=f"bob@{PASS_DOMAIN}",
            rcpt_to=recipient["username"],
            subject=subject,
            body_text=f"Hello from the {body_marker} test.\n",
        )
    except RuntimeError as e:
        raise AssertionError(
            f"ALIGNED-PASS REJECTED: an SPF-aligned-pass inbound from {PASS_DOMAIN} "
            f"must clear the enforced p=reject gate and be accepted (250). A reject "
            f"here means SPF/DMARC did not pass over the loopback delivery.\n{e}"
            f"{diag()}") from e

    # (read back, decrypted) IMAPS AUTH PLAIN → SELECT INBOX → UID FETCH BODY[].
    try:
        raw = imap_fetch_only_inbox_message(
            "127.0.0.1", mp[993], DOMAIN,
            recipient["username"], recipient["password"],
        )
    except (AssertionError, TimeoutError) as e:
        raise AssertionError(f"{e}{diag()}") from e

    text = raw.decode("utf-8", errors="replace")
    assert subject in text, (
        f"decrypted body must carry the Subject; got first 400B: {text[:400]!r}{diag()}")
    assert body_marker in text, (
        f"decrypted body must carry the message text; got first 400B: {text[:400]!r}{diag()}")
    assert msg_id.strip("<>") in text, f"decrypted body must carry the Message-ID{diag()}"

    # (2, negative control — same fixture, same enforced p=reject) An inbound from
    # FAIL_DOMAIN (`v=spf1 -all`, no DKIM) fails DMARC and must be rejected at DATA
    # with `550 5.7.1 DMARC reject`. This proves enforcement is genuinely live
    # here — so the positive's acceptance above reflects a real PASS, not a fixture
    # where DMARC enforcement silently didn't apply.
    rc, output = deliver_inbound_attempt_loopback_curl(
        name,
        mail_from=f"mallory@{FAIL_DOMAIN}",
        rcpt_to=recipient["username"],
        subject="perimeter-pass negative control",
        body_text="This SPF-failing message must be rejected under p=reject.\n",
    )
    assert rc != 0, (
        f"NEGATIVE CONTROL FAILED: an SPF-failing inbound from {FAIL_DOMAIN} under "
        f"enforced p=reject was accepted (curl exited 0) — enforcement is not live "
        f"in this fixture, so the positive accept above is not meaningful. "
        f"Output: {output!r}{diag()}")
    assert "5.7.1" in output and "DMARC reject" in output, (
        f"the SPF-failing inbound must be rejected `550 5.7.1 DMARC reject`; got "
        f"curl output: {output!r}{diag()}")


def _sign_dkim(dkim_keypair, *, mail_from, rcpt_to, subject, body_text):
    """Build an RFC 5322 message and DKIM-sign it (relaxed/relaxed, rsa-sha256)
    with the module keypair, returning ``(signed_bytes, message_id)``.

    Signs with dkimpy (the independent external signer) over the canonical
    From/To/Subject/Date/Message-ID set. relaxed/relaxed canonicalization survives
    the LF↔CRLF normalization curl→MTA may apply, so the body hash still verifies
    on the receiving ``mail-auth`` side."""
    msg_id = f"<{uuid.uuid4().hex}@e2e.test>"
    msg = email.message.EmailMessage()
    msg["From"] = mail_from
    msg["To"] = rcpt_to
    msg["Subject"] = subject
    msg["Message-ID"] = msg_id
    msg["Date"] = email.utils.formatdate(localtime=False, usegmt=True)
    msg.set_content(body_text)
    raw = msg.as_bytes()
    sig = dkim.sign(
        message=raw,
        selector=DKIM_SELECTOR.encode(),
        domain=DKIM_DOMAIN.encode(),
        privkey=dkim_keypair["priv_pem"],
        canonicalize=(b"relaxed", b"relaxed"),
        include_headers=[b"from", b"to", b"subject", b"date", b"message-id"],
    )
    return sig + raw, msg_id


@pytest.mark.feature("mail-abuse-refused")
def test_perimeter_accepts_dkim_aligned_pass_rejects_broken(
        accepting_nest, dkim_keypair, run_seal_helper):
    """DKIM half of Gap-2b (positive + negative control in one fixture): an inbound
    whose ONLY passing aligned identifier is a valid DKIM signature clears the
    enforced ``p=reject`` gate (accepted + IMAP read), while the SAME message with a
    body tampered after signing — DKIM body-hash mismatch, SPF still ``-all`` — is
    rejected ``550 5.7.1 DMARC reject``.

    Isolation argument (mirrors the SPF half, alignment vector flipped to DKIM):
    ``DKIM_DOMAIN`` publishes ``v=spf1 -all`` so SPF can never align; under an
    enforced ``p=reject`` the only way to clear ``applyDMARCRejectGate`` is a
    DKIM-aligned pass. So a green accept proves the full external-DKIM →
    verify_inbound → DMARC(dkim-aligned) → accept path against the real image; the
    body-tamper negative proves the verifier checks the signature *cryptographically*
    (not mere presence), so the positive reflects a genuinely valid signature."""
    nest = accepting_nest
    name = nest["name"]
    mp = nest["mail_ports"]  # container_port -> host_port

    register_primary_domain(nest, DOMAIN)
    relax_spam_policy(nest)
    put_auth_policy(nest, enforce_dmarc=True, log_only=False)
    recipient = provision_mail_recipient(
        nest, run_seal_helper, domain=DOMAIN, local_part=RECIPIENT_LOCAL,
        password=RECIPIENT_PASSWORD,
    )

    bring_bridges_to_serving(name, nest, mp, DOMAIN)

    diag = lambda: f"\n\n── bridge diagnostics ──\n{bridge_diag(name)}"  # noqa: E731

    # (1, positive) Validly DKIM-signed + aligned from DKIM_DOMAIN. SPF `-all`
    # cannot align, so clearing the enforced p=reject gate is possible only via the
    # DKIM-aligned pass — a green read proves the DKIM→DMARC→accept path.
    subject = "perimeter dkim aligned accept"
    body_marker = "deploy-image perimeter DKIM round-trip"
    signed, msg_id = _sign_dkim(
        dkim_keypair,
        mail_from=f"alice@{DKIM_DOMAIN}",
        rcpt_to=recipient["username"],
        subject=subject,
        body_text=f"Hello from the {body_marker} test.\n",
    )
    rc, out = deliver_inbound_raw_loopback_curl(
        name, mail_from=f"alice@{DKIM_DOMAIN}", rcpt_to=recipient["username"],
        raw=signed,
    )
    assert rc == 0, (
        f"DKIM-ALIGNED pass REJECTED: a validly DKIM-signed+aligned inbound from "
        f"{DKIM_DOMAIN} (SPF -all, so DKIM is the only alignment path) must clear "
        f"the enforced p=reject gate and be accepted. curl exit {rc}, output: "
        f"{out!r}{diag()}")

    # (read back, decrypted) IMAPS AUTH PLAIN → SELECT INBOX → UID FETCH BODY[].
    try:
        raw = imap_fetch_only_inbox_message(
            "127.0.0.1", mp[993], DOMAIN,
            recipient["username"], recipient["password"],
        )
    except (AssertionError, TimeoutError) as e:
        raise AssertionError(f"{e}{diag()}") from e

    text = raw.decode("utf-8", errors="replace")
    assert subject in text, (
        f"decrypted body must carry the Subject; got first 400B: {text[:400]!r}{diag()}")
    assert body_marker in text, (
        f"decrypted body must carry the message text; got first 400B: {text[:400]!r}{diag()}")
    assert msg_id.strip("<>") in text, f"decrypted body must carry the Message-ID{diag()}"

    # (2, negative control — same fixture, same key/selector) Corrupt a SIGNED
    # header value AFTER signing: `Subject` is in the DKIM `h=` set, so altering it
    # diverges the relaxed header hash from what `b=` signed → the RSA-over-headers
    # verification fails (the EXACT path the positive above passed). A header tamper
    # is transport-safe and canonicalization-unambiguous — unlike a trailing-body
    # edit, which the MDA's inbound body-hash path tolerated.
    # SPF is still `-all`, so DMARC has no passing aligned identifier → rejected
    # `550 5.7.1 DMARC reject`.
    neg_subject = "perimeter dkim negative control"
    signed_bad, _ = _sign_dkim(
        dkim_keypair,
        mail_from=f"alice@{DKIM_DOMAIN}",
        rcpt_to=recipient["username"],
        subject=neg_subject,
        body_text="negative control body\n",
    )
    tampered = signed_bad.replace(
        f"Subject: {neg_subject}".encode(),
        b"Subject: perimeter dkim TAMPERED control", 1)
    assert tampered != signed_bad, "subject-tamper must alter the signed bytes"
    rc, output = deliver_inbound_raw_loopback_curl(
        name, mail_from=f"alice@{DKIM_DOMAIN}", rcpt_to=recipient["username"],
        raw=tampered,
    )
    assert rc != 0, (
        f"NEGATIVE CONTROL FAILED: a header-tampered (broken-DKIM) inbound from "
        f"{DKIM_DOMAIN} under enforced p=reject was accepted (curl exit 0) — the "
        f"verifier is not checking the signature cryptographically, so the positive "
        f"accept above is not meaningful. Output: {output!r}{diag()}")
    assert "5.7.1" in output and "DMARC reject" in output, (
        f"the broken-DKIM inbound must be rejected `550 5.7.1 DMARC reject`; got "
        f"curl output: {output!r}{diag()}")

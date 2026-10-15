"""tier_4 witnesses for the DNS-dependent mail perimeter knobs an admin saves.

`test_mail_policy_effects.py` witnesses what a mail-policy save does to mail
against the standalone `mail_bridge_mta`, but four `[nest]` outcomes of
`docs/features/admin-mail-policy.md` need DNS answers that venue cannot give: it
dials from 127.0.0.1 and resolves through the dev machine's own resolver. This
module runs them against the deploy image in `mail_perimeter.perimeter_venue`,
where both the resolver and the peer's address are controllable (that module's
docstring says how).

Outcome 26 is about the DATA-time sender checks, which run the same on a loopback
delivery, so it uses the in-container `curl` path of
`test_mail_security_accept.py`, with the sidecar publishing SPF, DMARC and DKIM
records.

Spec: `docs/goal/behavior/mail-policy-config.md` § Inbound perimeter, § Inbound
authentication enforcement (which knob exists); `docs/goal/behavior/smtp-server.md`
§ Error / tempfail strategy (the reply each knob produces).
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

from .helpers import deliver_inbound_raw_loopback_curl, put_auth_policy
from .mail_perimeter import (
    LIAR_NAME,
    PEER_NAME,
    SENDER_DOMAIN,
    mail_from_reply,
    perimeter_venue,
    poll,
    publish,
    put_spam_policy,
    reversed_ip,
    session_opening,
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

CLAIM_CODE = "PERDNS"
RECIPIENT_LOCAL = "perimeter-user"

BLOCKLIST = "bl.example.test"            # lists the peer
CLEAN_LIST = "clean.example.test"        # lists nobody

# Outcome 26's sender domains (loopback delivery: the client IP is 127.0.0.1).
DMARC_FAIL_DOMAIN = "dmarc-fail.test"    # SPF -all + p=reject
SPF_FAIL_DOMAIN = "spf-fail.test"        # SPF -all, no DMARC record
DKIM_DOMAIN = "dkim-broken.test"         # SPF pass, no DMARC, a DKIM key
DKIM_SELECTOR = "e2e"


def _put_auth_policy(nest, *, log_only=False, enforce_dmarc=True,
                     enforce_spf_hardfail=True, enforce_dkim=False) -> None:
    put_auth_policy(nest, log_only=log_only, enforce_dmarc=enforce_dmarc,
                    enforce_spf_hardfail=enforce_spf_hardfail, enforce_dkim=enforce_dkim)


# ── Fixtures ──────────────────────────────────────────────────────────


@pytest.fixture(scope="module")
def dkim_keypair():
    """An RSA-2048 DKIM keypair: the PKCS8 private PEM for `dkim.sign` and the
    base64 SubjectPublicKeyInfo DER the published key record carries."""
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


@pytest.fixture(scope="module")
def perimeter_nest(tmp_path_factory, dkim_keypair, run_seal_helper):
    """The shared perimeter venue, its resolver publishing outcome 26's SPF,
    DMARC and DKIM records, with one mail recipient."""
    with perimeter_venue(
        tmp_path_factory, run_seal_helper, label="perdns", claim_code=CLAIM_CODE,
        recipients=(RECIPIENT_LOCAL,),
        txt_records={
            DMARC_FAIL_DOMAIN: "v=spf1 -all",
            f"_dmarc.{DMARC_FAIL_DOMAIN}": "v=DMARC1; p=reject",
            SPF_FAIL_DOMAIN: "v=spf1 -all",
            DKIM_DOMAIN: "v=spf1 ip4:127.0.0.1 -all",
            f"{DKIM_SELECTOR}._domainkey.{DKIM_DOMAIN}":
                f"v=DKIM1; k=rsa; p={dkim_keypair['pub_b64']}",
        },
    ) as nest:
        yield {**nest, "recipient": nest["recipients"][RECIPIENT_LOCAL]}


def _publish(nest, *, ptr_name: str | None) -> None:
    """Publish the host peer's DNS plus a blocklist that lists it."""
    publish(nest, ptr_name=ptr_name,
            a={f"{reversed_ip(nest['gateway'])}.{BLOCKLIST}": "127.0.0.2"})


# ── Outcome 12 ────────────────────────────────────────────────────────


@pytest.mark.feature("admin-mail-policy")
def test_server_on_an_admin_blocklist_is_refused_at_the_door(perimeter_nest):
    """A blocklist the peer is not on admits it (its greeting is answered 250);
    once the admin's list names a blocklist that lists the peer, the connection
    is refused `554 5.7.1` before any mail command."""
    nest = perimeter_nest
    port = nest["mail_ports"][25]
    _publish(nest, ptr_name=PEER_NAME)
    opening = lambda: session_opening(port, helo=PEER_NAME)  # noqa: E731

    put_spam_policy(nest, dnsbl_servers=[CLEAN_LIST])
    poll(opening, lambda r: r.startswith("250"),
          "a peer on no configured blocklist is admitted", nest["diag"])

    put_spam_policy(nest, dnsbl_servers=[CLEAN_LIST, BLOCKLIST])
    refused = poll(opening, lambda r: not r.startswith("250"),
                    "a peer listed on the admin's blocklist is refused", nest["diag"])
    assert refused.startswith("554 5.7.1"), (
        f"a blocklisted peer must be refused `554 5.7.1`; got {refused!r}{nest['diag']()}")


# ── Outcome 14 ────────────────────────────────────────────────────────


@pytest.mark.feature("admin-mail-policy")
def test_identity_checks_refuse_a_mismatched_greeting_or_reverse_lookup(perimeter_nest):
    """With the greeting-name check on, a greeting name that does not resolve to
    the peer is refused `554 5.7.0` while a matching one is accepted; with the
    reverse-lookup check enforcing, a reverse name whose address is not the
    peer's is refused `550 5.7.25` while a forward-confirmed one is accepted."""
    nest = perimeter_nest
    port = nest["mail_ports"][25]
    mail_from = f"someone@{SENDER_DOMAIN}"
    _publish(nest, ptr_name=PEER_NAME)

    # Greeting name: on → a name pointing elsewhere is refused at MAIL FROM.
    put_spam_policy(nest, helo_identity_required=True)
    refused = poll(lambda: mail_from_reply(port, helo=LIAR_NAME, mail_from=mail_from),
                    lambda r: not r.startswith("250"),
                    "a greeting name that does not resolve to the peer is refused",
                    nest["diag"])
    assert refused.startswith("554 5.7.0"), (
        f"a mismatched greeting name must be refused `554 5.7.0`; got {refused!r}{nest['diag']()}")
    accepted = mail_from_reply(port, helo=PEER_NAME, mail_from=mail_from)
    assert accepted.startswith("250"), (
        f"a greeting name resolving to the peer must pass the same check; got {accepted!r}"
        f"{nest['diag']()}")

    # Reverse lookup: enforce + reject → a PTR that does not forward-confirm is
    # refused; the greeting check is off so only the reverse lookup decides.
    put_spam_policy(nest, fcrdns_mode="enforce", reject_fcrdns_fail=True)
    _publish(nest, ptr_name=LIAR_NAME)
    refused = poll(lambda: mail_from_reply(port, helo=PEER_NAME, mail_from=mail_from),
                    lambda r: not r.startswith("250"),
                    "a reverse name that does not resolve back to the peer is refused",
                    nest["diag"])
    assert refused.startswith("550 5.7.25"), (
        f"a reverse lookup that does not match must be refused `550 5.7.25`; got {refused!r}"
        f"{nest['diag']()}")
    _publish(nest, ptr_name=PEER_NAME)
    accepted = poll(lambda: mail_from_reply(port, helo=PEER_NAME, mail_from=mail_from),
                     lambda r: r.startswith("250"),
                     "a forward-confirmed reverse name passes the enforcing check",
                     nest["diag"])
    assert accepted.startswith("250")


# ── Outcome 15 ────────────────────────────────────────────────────────


@pytest.mark.feature("admin-mail-policy")
def test_reject_on_no_reverse_name_refuses_a_server_without_one(perimeter_nest):
    """A peer with no reverse name is accepted while the knob is off; on, it is
    refused `550 5.7.25` at MAIL FROM; and the same knob admits the peer once it
    has a reverse name — the absence is what it refuses."""
    nest = perimeter_nest
    port = nest["mail_ports"][25]
    mail_from = f"someone@{SENDER_DOMAIN}"
    _publish(nest, ptr_name=None)

    put_spam_policy(nest)
    accepted = poll(lambda: mail_from_reply(port, helo=PEER_NAME, mail_from=mail_from),
                     lambda r: r.startswith("250"),
                     "with the knob off a peer without a reverse name is accepted",
                     nest["diag"])
    assert accepted.startswith("250")

    put_spam_policy(nest, reject_no_rdns=True)
    refused = poll(lambda: mail_from_reply(port, helo=PEER_NAME, mail_from=mail_from),
                    lambda r: not r.startswith("250"),
                    "with the knob on a peer without a reverse name is refused",
                    nest["diag"])
    assert refused.startswith("550 5.7.25"), (
        f"a peer with no reverse name must be refused `550 5.7.25`; got {refused!r}"
        f"{nest['diag']()}")

    _publish(nest, ptr_name=PEER_NAME)
    accepted = poll(lambda: mail_from_reply(port, helo=PEER_NAME, mail_from=mail_from),
                     lambda r: r.startswith("250"),
                     "with the knob on a peer that has a reverse name is accepted",
                     nest["diag"])
    assert accepted.startswith("250")


# ── Outcome 26 ────────────────────────────────────────────────────────


def _message(*, mail_from: str, rcpt_to: str, subject: str) -> bytes:
    msg = email.message.EmailMessage()
    msg["From"] = mail_from
    msg["To"] = rcpt_to
    msg["Subject"] = subject
    msg["Message-ID"] = f"<{uuid.uuid4().hex}@e2e.test>"
    msg["Date"] = email.utils.formatdate(localtime=False, usegmt=True)
    msg.set_content("perimeter sender-check witness\n")
    return msg.as_bytes()


def _broken_dkim_message(dkim_keypair, *, mail_from: str, rcpt_to: str) -> bytes:
    """A message dkimpy signs for `DKIM_DOMAIN`, then a signed header altered —
    the signature is present and fails verification."""
    raw = _message(mail_from=mail_from, rcpt_to=rcpt_to, subject="dkim signed subject")
    sig = dkim.sign(
        message=raw,
        selector=DKIM_SELECTOR.encode(),
        domain=DKIM_DOMAIN.encode(),
        privkey=dkim_keypair["priv_pem"],
        canonicalize=(b"relaxed", b"relaxed"),
        include_headers=[b"from", b"to", b"subject", b"date", b"message-id"],
    )
    signed = sig + raw
    tampered = signed.replace(b"Subject: dkim signed subject", b"Subject: dkim altered subject", 1)
    assert tampered != signed, "the subject tamper must alter the signed bytes"
    return tampered


@pytest.mark.feature("admin-mail-policy")
def test_log_only_or_a_check_switched_off_accepts_and_dkim_enforcement_refuses(
        perimeter_nest, dkim_keypair):
    """Each sender check flips on its own knob, observed as the DATA reply:
    a DMARC `p=reject` failure is refused `550 5.7.1` and accepted under log-only;
    an SPF hard fail is refused `550 5.7.23` and accepted with that check off; a
    failing DKIM signature is accepted with signature enforcement off and refused
    `550 5.7.20` with it on."""
    nest = perimeter_nest
    name = nest["name"]
    rcpt = nest["recipient"]["username"]
    put_spam_policy(nest)

    def deliver(sender_domain: str, raw: bytes | None = None):
        mail_from = f"sender@{sender_domain}"
        body = raw or _message(mail_from=mail_from, rcpt_to=rcpt, subject="sender check")
        return deliver_inbound_raw_loopback_curl(
            name, mail_from=mail_from, rcpt_to=rcpt, raw=body)

    accepted = lambda r: r[0] == 0  # noqa: E731
    refused = lambda r: r[0] != 0  # noqa: E731

    # DMARC: enforced → refused; log-only → the same message accepted.
    _put_auth_policy(nest)
    rc, out = poll(lambda: deliver(DMARC_FAIL_DOMAIN), refused,
                    "a DMARC p=reject failure is refused under enforcement", nest["diag"])
    assert "550 5.7.1" in out and "DMARC reject" in out, (
        f"expected `550 5.7.1 DMARC reject`; got {out!r}{nest['diag']()}")
    _put_auth_policy(nest, log_only=True)
    poll(lambda: deliver(DMARC_FAIL_DOMAIN), accepted,
          "under log-only the DMARC-failing message is accepted", nest["diag"])

    # SPF hard fail (no DMARC record, so the SPF check decides): on → refused;
    # switched off → accepted.
    _put_auth_policy(nest)
    rc, out = poll(lambda: deliver(SPF_FAIL_DOMAIN), refused,
                    "an SPF hard fail is refused with the SPF check on", nest["diag"])
    assert "550 5.7.23" in out, f"expected `550 5.7.23 SPF reject`; got {out!r}{nest['diag']()}"
    _put_auth_policy(nest, enforce_spf_hardfail=False)
    poll(lambda: deliver(SPF_FAIL_DOMAIN), accepted,
          "with the SPF check off the hard-failing message is accepted", nest["diag"])

    # DKIM (SPF passes, no DMARC record): enforcement off → a failing signature
    # is accepted; on → refused.
    broken = lambda: _broken_dkim_message(  # noqa: E731
        dkim_keypair, mail_from=f"sender@{DKIM_DOMAIN}", rcpt_to=rcpt)
    _put_auth_policy(nest, enforce_dkim=False)
    poll(lambda: deliver(DKIM_DOMAIN, broken()), accepted,
          "with signature enforcement off a failing signature is accepted", nest["diag"])
    _put_auth_policy(nest, enforce_dkim=True)
    rc, out = poll(lambda: deliver(DKIM_DOMAIN, broken()), refused,
                    "with signature enforcement on a failing signature is refused",
                    nest["diag"])
    assert "550 5.7.20" in out, (
        f"expected `550 5.7.20 DKIM signature missing or invalid`; got {out!r}{nest['diag']()}")

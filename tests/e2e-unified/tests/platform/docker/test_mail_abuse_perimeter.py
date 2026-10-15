"""tier_4 witnesses for the mail-abuse-refused outcomes a loopback peer cannot see.

`test_mail_abuse_refused.py` witnesses the rest of
`docs/features/mail-abuse-refused.md` against the process-level bridge, from
127.0.0.1. The bridge exempts loopback from the greeting-name rules, the sender
domain lookup, the blocklist and both per-IP caps (`internal/mta/policy.go`,
`server.go`), and resolves through the machine's own resolver, so these
outcomes run against the deploy image in `mail_perimeter.perimeter_venue`: the
peer arrives as the network gateway and every name it presents resolves through
a `fake_dns` sidecar the test publishes into.

Outcomes 5 and 6 are about the DATA-time sender checks, which run the same on a
loopback delivery, so they use the in-container `curl` path, with the sidecar
publishing SPF and DMARC records.

The port-25 per-IP message rate (outcome 14) is held for the bridge process's
life and counts every accepted MAIL FROM from the gateway, so its witness opens
a venue of its own; the shared one here stays far below the budget.

Spec: `docs/goal/behavior/smtp-server.md` § HELO / EHLO syntactic + identity
validation, § Error / tempfail strategy, § Connection-time limits;
`docs/goal/behavior/mail-policy-config.md` § Inbound perimeter (which gates
have a knob, which are constants).
"""

import email.message
import email.utils
import socket
import subprocess
import uuid

import pytest

from .helpers import (
    add_local_domain,
    deliver_inbound_raw_loopback_curl,
    imap_fetch_only_inbox_message,
    put_auth_policy,
)
from .mail_perimeter import (
    DOMAIN,
    PEER_NAME,
    RECIPIENT_PASSWORD,
    SENDER_DOMAIN,
    mail_from_replies,
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

CLAIM_CODE = "ABUSEP"
RECIPIENT_LOCAL = "abuse-user"
JUNK_RECIPIENT_LOCAL = "abuse-junk-user"

# The shipped default blocklist (`libs/fauna-mail/src/deliverability.rs`).
DEFAULT_BLOCKLIST = "zen.spamhaus.org"
# A domain the nest hosts mail for besides the `localhost` primary: an FQDN, so
# a greeting with it passes every syntactic rule and only the self-claim rule
# can refuse it.
HOSTED_DOMAIN = "hosted.example.test"
# Sender domains with no records at all (NXDOMAIN on every lookup).
NO_SUCH_DOMAIN = "no-such-sender.example.test"
# Names whose every lookup answers SERVFAIL (outcome 10).
UNANSWERED_NAME = "unanswered-peer.example.test"
UNANSWERED_SENDER = "unanswered-sender.example.test"

# Outcomes 5 and 6's sender domains (loopback delivery: the client IP is
# 127.0.0.1, which none of these SPF records authorizes).
QUARANTINE_DOMAIN = "dmarc-quarantine.test"  # SPF -all + p=quarantine
SPF_FAIL_DOMAIN = "spf-hardfail.test"        # SPF -all, no DMARC record
SPF_SOFTFAIL_DOMAIN = "spf-softfail.test"    # SPF ~all, no DMARC record
SPF_NEUTRAL_DOMAIN = "spf-neutral.test"      # SPF ?all, no DMARC record


@pytest.fixture(scope="module")
def abuse_nest(tmp_path_factory, run_seal_helper):
    """The shared perimeter venue, its resolver publishing outcomes 5 and 6's
    SPF and DMARC records, with two mail recipients (one read only for Junk)."""
    with perimeter_venue(
        tmp_path_factory, run_seal_helper, label="abuse", claim_code=CLAIM_CODE,
        recipients=(RECIPIENT_LOCAL, JUNK_RECIPIENT_LOCAL),
        txt_records={
            QUARANTINE_DOMAIN: "v=spf1 -all",
            f"_dmarc.{QUARANTINE_DOMAIN}": "v=DMARC1; p=quarantine",
            SPF_FAIL_DOMAIN: "v=spf1 -all",
            SPF_SOFTFAIL_DOMAIN: "v=spf1 ~all",
            SPF_NEUTRAL_DOMAIN: "v=spf1 ?all",
        },
    ) as nest:
        yield nest


def _message(*, mail_from: str, rcpt_to: str, subject: str) -> bytes:
    msg = email.message.EmailMessage()
    msg["From"] = mail_from
    msg["To"] = rcpt_to
    msg["Subject"] = subject
    msg["Message-ID"] = f"<{uuid.uuid4().hex}@e2e.test>"
    msg["Date"] = email.utils.formatdate(localtime=False, usegmt=True)
    msg.set_content("mail-abuse-refused perimeter witness\n")
    return msg.as_bytes()


def _deliver(nest, *, sender_domain: str, rcpt_to: str, subject: str) -> tuple[int, str]:
    mail_from = f"sender@{sender_domain}"
    return deliver_inbound_raw_loopback_curl(
        nest["name"], mail_from=mail_from, rcpt_to=rcpt_to,
        raw=_message(mail_from=mail_from, rcpt_to=rcpt_to, subject=subject))


# ── Outcome 5 ─────────────────────────────────────────────────────────


@pytest.mark.feature("mail-abuse-refused")
def test_a_quarantine_policy_failure_is_accepted_and_filed_as_spam(abuse_nest):
    """Mail failing its sender domain's checks, where the domain publishes
    `p=quarantine`, is accepted (never `550 5.7.1 DMARC reject`) and reaches the
    recipient's Junk folder, read back over IMAPS."""
    nest = abuse_nest
    recipient = nest["recipients"][JUNK_RECIPIENT_LOCAL]
    subject = f"quarantine witness {uuid.uuid4().hex[:8]}"
    put_auth_policy(nest, log_only=False, enforce_dmarc=True,
                    enforce_spf_hardfail=True, enforce_dkim=False)

    rc, out = _deliver(nest, sender_domain=QUARANTINE_DOMAIN,
                       rcpt_to=recipient["username"], subject=subject)
    assert rc == 0, (
        f"a p=quarantine failure must be accepted, not refused; curl exit {rc}: {out!r}"
        f"{nest['diag']()}")

    try:
        raw = imap_fetch_only_inbox_message(
            "127.0.0.1", nest["mail_ports"][993], DOMAIN,
            recipient["username"], RECIPIENT_PASSWORD, mailbox="Junk")
    except (AssertionError, TimeoutError) as e:
        raise AssertionError(f"{e}{nest['diag']()}") from e
    assert subject.encode() in raw, (
        f"the quarantined message must be the one filed to Junk; got {raw[:400]!r}")


# ── Outcome 6 ─────────────────────────────────────────────────────────


@pytest.mark.feature("mail-abuse-refused")
def test_an_spf_hard_fail_is_refused_and_a_softer_answer_never_is(abuse_nest):
    """With no DMARC record to decide, a sender whose domain's SPF says `-all`
    is refused `550 5.7.23`; the same message from a domain whose SPF answers
    softfail (`~all`) or neutral (`?all`) is accepted."""
    nest = abuse_nest
    rcpt = nest["recipients"][RECIPIENT_LOCAL]["username"]
    put_auth_policy(nest, log_only=False, enforce_dmarc=True,
                    enforce_spf_hardfail=True, enforce_dkim=False)

    rc, out = poll(lambda: _deliver(nest, sender_domain=SPF_FAIL_DOMAIN, rcpt_to=rcpt,
                                    subject="spf hard fail"),
                   lambda r: r[0] != 0,
                   "an SPF hard fail with no DMARC record is refused", nest["diag"])
    assert "550 5.7.23" in out, f"expected `550 5.7.23 SPF reject`; got {out!r}{nest['diag']()}"

    for domain in (SPF_SOFTFAIL_DOMAIN, SPF_NEUTRAL_DOMAIN):
        rc, out = _deliver(nest, sender_domain=domain, rcpt_to=rcpt, subject=f"spf {domain}")
        assert rc == 0, (
            f"an SPF answer softer than fail ({domain}) must never be refused; curl exit "
            f"{rc}: {out!r}{nest['diag']()}")


# ── Outcome 7 ─────────────────────────────────────────────────────────


@pytest.mark.feature("mail-abuse-refused")
def test_an_empty_garbled_localhost_or_single_word_greeting_is_refused(abuse_nest):
    """A sending server greeting with nothing, with a garbled name, with
    `localhost` or with a bare single-word name is refused; the same session
    greeting with a real name is accepted. The syntactic rules are always on —
    every DNS gate is off here."""
    nest = abuse_nest
    port = nest["mail_ports"][25]
    mail_from = f"someone@{SENDER_DOMAIN}"
    publish(nest)
    put_spam_policy(nest)

    accepted = poll(lambda: mail_from_reply(port, helo=PEER_NAME, mail_from=mail_from),
                    lambda r: r.startswith("250"),
                    "a sender greeting with a real name is accepted", nest["diag"])
    assert accepted.startswith("250")

    for helo in ("localhost", "LOCALHOST.localdomain", "peerhost"):
        refused = mail_from_reply(port, helo=helo, mail_from=mail_from)
        assert refused.startswith("554 5.7.0"), (
            f"a greeting as {helo!r} must be refused `554 5.7.0`; got {refused!r}"
            f"{nest['diag']()}")
    for helo in ("", "peer\x01.example.test"):
        refused = mail_from_reply(port, helo=helo, mail_from=mail_from)
        # Refused at MAIL FROM, or — the empty greeting — at the greeting itself.
        assert refused.removeprefix("EHLO ").startswith("5"), (
            f"a greeting as {helo!r} must be refused with a permanent reply; got {refused!r}"
            f"{nest['diag']()}")


# ── Outcome 8 ─────────────────────────────────────────────────────────


# Not yet marked for the feature: the rule was built 2026-10-13, after the
# newest image this machine holds, so the witness waits for an image built from
# a tree that contains it (`docs/features/mail-abuse-refused.md` outcome 8).
def test_a_greeting_with_the_nests_own_name_is_refused(tmp_path_factory, run_seal_helper):
    """A sending server greeting with a domain this nest hosts mail for is
    refused `554 5.7.0` as an impersonation — even with the greeting-name
    identity check off and the name resolving to the peer, so nothing but the
    self-claim rule can refuse it — while a foreign name passes the same way.
    A venue of its own: until the saved domain applies, every probe is an
    accepted transaction spending the per-IP message budget."""
    with perimeter_venue(tmp_path_factory, run_seal_helper, label="selfclaim",
                         claim_code="SELFCL", recipients=()) as nest:
        port = nest["mail_ports"][25]
        mail_from = f"someone@{SENDER_DOMAIN}"
        add_local_domain(nest, HOSTED_DOMAIN)
        publish(nest, a={HOSTED_DOMAIN: nest["gateway"]})
        put_spam_policy(nest)

        refused = poll(lambda: mail_from_reply(port, helo=HOSTED_DOMAIN, mail_from=mail_from),
                       lambda r: not r.startswith("250"),
                       "a greeting with a hosted domain is refused", nest["diag"])
        assert refused.startswith("554 5.7.0") and "own name" in refused, (
            f"a greeting with the nest's own name must be refused `554 5.7.0` as a "
            f"self-claim; got {refused!r}{nest['diag']()}")
        accepted = mail_from_reply(port, helo=PEER_NAME, mail_from=mail_from)
        assert accepted.startswith("250"), (
            f"a foreign name must pass where the nest's own is refused; got {accepted!r}"
            f"{nest['diag']()}")


# ── Outcome 9 ─────────────────────────────────────────────────────────


@pytest.mark.feature("mail-abuse-refused")
def test_a_sender_domain_that_does_not_exist_or_is_missing_is_refused(abuse_nest):
    """A sender address naming a domain with no MX and no address record is
    refused `550 5.7.1`; a sender address with no domain at all is refused as
    invalid; a sender domain that resolves is accepted. The lookup is always
    on — every DNS gate the admin owns is off here."""
    nest = abuse_nest
    port = nest["mail_ports"][25]
    publish(nest)
    put_spam_policy(nest)

    accepted = poll(lambda: mail_from_reply(port, helo=PEER_NAME,
                                            mail_from=f"someone@{SENDER_DOMAIN}"),
                    lambda r: r.startswith("250"),
                    "a sender domain that resolves is accepted", nest["diag"])
    assert accepted.startswith("250")

    refused = mail_from_reply(port, helo=PEER_NAME, mail_from=f"someone@{NO_SUCH_DOMAIN}")
    assert refused.startswith("550 5.7.1"), (
        f"a sender domain with no MX or A record must be refused `550 5.7.1`; got "
        f"{refused!r}{nest['diag']()}")
    # No domain at all: the SMTP library's path parser refuses what it cannot
    # parse with its own `501 5.5.2`; the bridge's `554 5.1.7` answers the rest.
    for sender in ("someone@", "no-at-sign"):
        refused = mail_from_reply(port, helo=PEER_NAME, mail_from=sender)
        assert refused.startswith(("501 5.5.2", "554 5.1.7")), (
            f"a sender address with no domain ({sender!r}) must be refused as invalid; got "
            f"{refused!r}{nest['diag']()}")


# ── Outcome 10 ────────────────────────────────────────────────────────


@pytest.mark.feature("mail-abuse-refused")
def test_a_lookup_that_cannot_be_answered_never_refuses_mail(abuse_nest):
    """With the default blocklist, the greeting-name identity check and the
    reverse-name checks all enforcing, a peer whose every lookup is NXDOMAIN is
    refused — so each gate is live — and the same peer is accepted once every
    one of those lookups answers SERVFAIL: blocklist, reverse name, greeting
    name and sender domain alike fail open."""
    nest = abuse_nest
    port = nest["mail_ports"][25]
    rev = reversed_ip(nest["gateway"])
    publish(nest)
    put_spam_policy(nest, omit=("dnsbl_servers",), helo_identity_required=True,
                    fcrdns_mode="enforce", reject_fcrdns_fail=True, reject_no_rdns=True)

    refused = poll(lambda: mail_from_reply(port, helo=UNANSWERED_NAME,
                                           mail_from=f"someone@{UNANSWERED_SENDER}"),
                   lambda r: not r.startswith("250"),
                   "a greeting name with no address is refused while the gates enforce",
                   nest["diag"])
    assert refused.startswith("554 5.7.0"), (
        f"a greeting name that does not exist must be refused `554 5.7.0`; got {refused!r}"
        f"{nest['diag']()}")

    publish(nest, servfail=[
        f"{rev}.{DEFAULT_BLOCKLIST}",
        f"{rev}.in-addr.arpa",
        UNANSWERED_NAME,
        UNANSWERED_SENDER,
    ])
    accepted = mail_from_reply(port, helo=UNANSWERED_NAME,
                               mail_from=f"someone@{UNANSWERED_SENDER}")
    assert accepted.startswith("250"), (
        f"lookups answered SERVFAIL must fail open, never refuse; got {accepted!r}"
        f"{nest['diag']()}")


# ── Outcome 11 ────────────────────────────────────────────────────────


@pytest.mark.feature("mail-abuse-refused")
def test_the_default_blocklist_refuses_a_listed_server_at_the_door(abuse_nest):
    """With the blocklist setting never chosen (the put leaves it out, so the
    shipped `zen.spamhaus.org` applies), a peer the list does not name is
    admitted, and once the list names it the connection is refused `554 5.7.1`
    in reply to its greeting, before any mail command."""
    nest = abuse_nest
    port = nest["mail_ports"][25]
    listing = {f"{reversed_ip(nest['gateway'])}.{DEFAULT_BLOCKLIST}": "127.0.0.2"}
    opening = lambda: session_opening(port, helo=PEER_NAME)  # noqa: E731

    publish(nest)
    put_spam_policy(nest, omit=("dnsbl_servers",))
    publish(nest, a=listing)
    refused = poll(opening, lambda r: not r.startswith("250"),
                   "a peer the default blocklist names is refused", nest["diag"])
    assert refused.startswith("554 5.7.1") and DEFAULT_BLOCKLIST in refused, (
        f"a peer on the default blocklist must be refused `554 5.7.1` naming the list; "
        f"got {refused!r}{nest['diag']()}")

    publish(nest)
    admitted = opening()
    assert admitted.startswith("250"), (
        f"a peer the default blocklist does not name must be admitted; got {admitted!r}"
        f"{nest['diag']()}")


# ── Outcome 13 ────────────────────────────────────────────────────────


def _open_greeted(port: int) -> tuple[socket.socket, bytes]:
    """Open a TCP connection to port 25 and read what the server first says:
    its `220` greeting, or b"" when it closes the connection unanswered."""
    sock = socket.create_connection(("127.0.0.1", port), timeout=10)
    sock.settimeout(10)
    try:
        first = sock.recv(512)
    except (TimeoutError, ConnectionResetError):
        first = b""
    return sock, first


@pytest.mark.feature("mail-abuse-refused")
def test_one_address_cannot_hold_more_than_half_the_incoming_connections(abuse_nest):
    """Fifty connections held open from one address are each greeted `220`; the
    fifty-first is closed unanswered; once one of the fifty closes, a new one is
    greeted again — the cap, not the server, refused it. (The per-IP rate gate
    runs at the greeting, which these connections never send.)"""
    nest = abuse_nest
    port = nest["mail_ports"][25]
    put_spam_policy(nest)
    held: list[socket.socket] = []
    try:
        for i in range(50):
            sock, first = _open_greeted(port)
            held.append(sock)
            assert first.startswith(b"220"), (
                f"connection {i + 1} of 50 from one address must be greeted; got {first!r}"
                f"{nest['diag']()}")
        extra, first = _open_greeted(port)
        held.append(extra)
        assert not first.startswith(b"220"), (
            f"the 51st concurrent connection from one address must be closed unanswered; "
            f"got {first!r}{nest['diag']()}")

        held.pop(0).close()
        again = poll(lambda: _probe_greeting(port), lambda r: r.startswith(b"220"),
                     "a connection is greeted again once one of the fifty closes",
                     nest["diag"])
        assert again.startswith(b"220")
    finally:
        for sock in held:
            sock.close()


def _probe_greeting(port: int) -> bytes:
    sock, first = _open_greeted(port)
    sock.close()
    return first


# ── Outcome 14 ────────────────────────────────────────────────────────


@pytest.mark.feature("mail-abuse-refused")
def test_one_address_cannot_push_more_than_fifty_messages_an_hour(tmp_path_factory,
                                                                    run_seal_helper):
    """From a venue of its own (the budget is the bridge process's), one sending
    address gets fifty message transactions accepted on one connection and the
    fifty-first answered `451 4.7.1`."""
    with perimeter_venue(tmp_path_factory, run_seal_helper, label="msgrate",
                         claim_code="MSGRAT", recipients=()) as nest:
        publish(nest)
        replies = mail_from_replies(nest["mail_ports"][25], helo=PEER_NAME,
                                    mail_from=f"someone@{SENDER_DOMAIN}", count=51)
        accepted = [r for r in replies if r.startswith("250")]
        assert len(accepted) == 50 and len(replies) == 51, (
            f"fifty transactions from one address must be accepted; got {replies!r}"
            f"{nest['diag']()}")
        assert replies[-1].startswith("451 4.7.1"), (
            f"the fifty-first transaction in an hour must be answered `451 4.7.1`; got "
            f"{replies[-1]!r}{nest['diag']()}")

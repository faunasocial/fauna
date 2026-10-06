"""tier_4 (live-remote, OPT-IN): external inbound over **port 25** against a live
nest (e.g. example.com). This is the regression guard for the deployment bugs that
silently broke real external mail (an external sender → bounce), which neither
the UI-only receive test nor the IMAP-APPEND tests catch because they bypass the
real MX + port-25 perimeter:

  1. **MX correctness** — the MX record for `<domain>` must resolve to a real,
     resolvable host (the live bug published `mail.<apex>.<apex>` — a doubled,
     non-existent target — so every external sender got NXDOMAIN and bounced).
  2. **Port 25 SMTP serving + STARTTLS** on `mail.<domain>`.
  3. **The full external-SMTP ingest path** — a real `MAIL`/`RCPT`/`DATA`
     transaction over port 25 is accepted (`250`), sealed by the MTA, and the
     message is then readable over IMAP. (Covers the DNSBL error-sentinel bug:
     with the perimeter relaxed the delivery must be accepted, not 554'd.)

This is an INFRA test, NOT the UI-only `test_mail_zero_cheat_live`. A synthetic
test sender cannot pass the perimeter gates a real MTA passes (HELO
forward-confirm, DNSBL, FCrDNS), so the test RELAXES them first via the admin API
(setup, not the assertion) — `fauna.bridges.put_spam_policy`. The thing under
test is the MX/DNS + port-25 wire + ingest, not the (unit-tested) gate logic.

Preconditions (env):
  --nest live:URL --live-box disposable
                            THE OPT-IN — this test is DESTRUCTIVE (it overwrites the box's global spam policy and never restores it), so
                            it runs only on a run that declares its box
                            disposable, which only a staging box can be
                            (testing.md § The shared-box rule → The
                            disposable-box declaration). The box is that run's
                            (``live_box_door.destructive_live_box``) — an
                            exported FAUNA_LIVE_NEST_URL never picks it alone.
  admin seed                resolved per box (``live_box_door.admin_seed``:
                            FAUNA_LIVE_SECRET_HEX > the box's
                            ``~/.config/fauna/staging-box/<host>.json`` >
                            ``~/.fauna-id``); ambient, so never the opt-in.
  mailbox                   resolved per box (``live_box_door.mailbox``):
                            FAUNA_LIVE_MAIL_ADDRESS / FAUNA_LIVE_MAIL_PASSWORD
                            > the box's staging-box file ``handle`` /
                            ``mail_password`` (the identity it was provisioned
                            under); each var overrides its own field.
                            An existing, mail-enabled mailbox; the
                            password is the PLAIN one for the IMAP read-back.

Also requires **outbound TCP 25** from the test host to `mail.<domain>` (many
cloud hosts block outbound 25 — the test skips, not fails, if it can't connect).
"""

import imaplib
import smtplib
import socket
import ssl
import subprocess
import time

import pytest

from helpers import live_box_door

# DESTRUCTIVE (see the module docstring): the box is the run's
# declared-disposable one and the admin seed is resolved for THAT box
# (`live_box_door.destructive_live_box` / `admin_seed`). The seed is ambient —
# a staging-box file or `~/.fauna-id` — so it is never the opt-in; the run's
# `--live-box disposable` declaration is (testing.md § The shared-box rule).
URL, _NOT_DISPOSABLE = live_box_door.destructive_live_box()
SECRET, SECRET_SOURCE = live_box_door.admin_seed(URL)

ADDRESS, PASSWORD = live_box_door.mailbox(URL)

pytestmark = [
    pytest.mark.tier_4,
    pytest.mark.live_box,
    pytest.mark.cd_suite,
    pytest.mark.skipif(_NOT_DISPOSABLE is not None, reason=_NOT_DISPOSABLE or ""),
    pytest.mark.skipif(
        not (SECRET and ADDRESS and PASSWORD),
        reason="live port-25 inbound test: provide the box's admin seed "
        + f"({live_box_door.SEED_SOURCES}) and its mailbox ({live_box_door.MAILBOX_SOURCES}) (opt-in)",
    ),
]

DOMAIN = ADDRESS.split("@", 1)[-1] if "@" in ADDRESS else ADDRESS
MAIL_HOST = f"mail.{DOMAIN}"
SMTP_PORT = 25
IMAP_PORT = 993
# The MAIL FROM domain must RESOLVE — the MTA rejects mail from a non-existent
# sender domain (separate from the HELO/DNSBL/FCrDNS gates the policy below
# relaxes). Use the deployment's own (resolvable) domain; the path under test is
# the port-25 wire + ingest, not sender provenance. DMARC alignment isn't
# enforced in the relaxed-perimeter test posture.
SENDER = f"probe@{DOMAIN}"


def _dig_mx(domain: str) -> list[tuple[int, str]]:
    """Return [(priority, target)] for the domain's MX, via `dig` (no dnspython
    dependency). Targets keep their trailing-dot form as served."""
    out = subprocess.run(
        ["dig", "+short", "MX", domain], capture_output=True, text=True, timeout=20
    ).stdout
    mx = []
    for line in out.splitlines():
        parts = line.split()
        if len(parts) == 2 and parts[0].isdigit():
            mx.append((int(parts[0]), parts[1]))
    return mx


def _relax_perimeter() -> None:
    """Open the spam perimeter so a synthetic test sender is accepted (setup, via
    admin WS-RPC — NOT part of the assertion). Real senders pass these gates; a
    test host can't (no forward-confirmed HELO, no DNSBL/FCrDNS standing)."""
    from nacl.signing import SigningKey

    from clients.ws_rpc_admin_client import WsRpcAdminClient

    sk = SigningKey(bytes.fromhex(SECRET))
    with WsRpcAdminClient(URL, actor_id=bytes(sk.verify_key), signing_key=bytes(sk)) as adm:
        adm.call(
            "fauna.bridges.put_spam_policy",
            {
                "baseline_standing_publish": False,
                "dnsbl_servers": [],
                "greylist_enabled": False,
                "greylist_delay_secs": 0,
                "fcrdns_mode": "off",
                "helo_identity_required": False,
                "max_conn_per_min": 1000,
            },
        )


def _deliver_port25(nonce: str, deadline: float) -> None:
    """One real inbound SMTP transaction over port 25 + STARTTLS. Retries until
    accepted or the deadline — the bridge applies the relaxed policy via its
    config-changed push a beat after `put_spam_policy`."""
    msg = (
        "\r\n".join([
            f"From: Probe <{SENDER}>",
            f"To: {ADDRESS}",
            f"Subject: {nonce}",
            "MIME-Version: 1.0",
            "Content-Type: text/plain; charset=utf-8",
            "",
            f"Port-25 external inbound probe {nonce}.",
        ]) + "\r\n"
    )
    last = ""
    while time.monotonic() < deadline:
        try:
            s = smtplib.SMTP(MAIL_HOST, SMTP_PORT, timeout=20)
            try:
                s.ehlo("probe.example")
                if s.has_extn("starttls"):
                    s.starttls(context=ssl.create_default_context())
                    s.ehlo("probe.example")
                code, resp = s.mail(SENDER)
                if code != 250:
                    last = f"MAIL {code} {resp!r}"
                else:
                    code, resp = s.rcpt(ADDRESS)
                    if code != 250:
                        last = f"RCPT {code} {resp!r}"
                    else:
                        code, resp = s.data(msg)
                        if code == 250:
                            return
                        last = f"DATA {code} {resp!r}"
            finally:
                try:
                    s.quit()
                except Exception:
                    pass
        except smtplib.SMTPResponseException as e:
            last = f"{e.smtp_code} {e.smtp_error!r}"
        except OSError as e:
            last = str(e)
        time.sleep(4.0)
    pytest.fail(f"port-25 delivery to {ADDRESS} never accepted (last: {last})")


@pytest.mark.feature("mail-server")
def test_port25_external_inbound_live():
    # Skip (not fail) when outbound 25 is blocked — common on cloud hosts.
    try:
        with socket.create_connection((MAIL_HOST, SMTP_PORT), timeout=10):
            pass
    except OSError as e:
        pytest.skip(f"outbound TCP 25 to {MAIL_HOST} unavailable from this host ({e})")

    # The perimeter relax is admin-gated: a box that does not know the resolved
    # seed skips as environment, naming the seed's source.
    live_box_door.preflight_admin(URL, SECRET, SECRET_SOURCE)

    # 1) MX correctness — must exist, not be doubled, and resolve to an A record.
    mx = _dig_mx(DOMAIN)
    assert mx, f"{DOMAIN} has no MX record"
    _, target = sorted(mx)[0]
    target_host = target.rstrip(".")
    doubled = f"{MAIL_HOST}.{DOMAIN}"
    assert target_host != doubled, (
        f"MX target is doubled ({target_host}) — a relative MX value got the zone "
        f"origin appended; external senders get NXDOMAIN and bounce."
    )
    try:
        socket.getaddrinfo(target_host, None)
    except OSError as e:
        pytest.fail(f"MX target {target_host!r} does not resolve ({e}) — inbound bounces")

    # 2) Relax the perimeter (setup) so the synthetic sender is accepted.
    _relax_perimeter()

    # 3) Deliver over port 25 (+ STARTTLS) → must be accepted (250).
    nonce = f"port25{int(time.time())}qx"
    _deliver_port25(nonce, time.monotonic() + 90.0)

    # 4) Read it back over IMAP — proves the MTA sealed it and the MDA serves it.
    ctx = ssl.create_default_context()
    deadline = time.monotonic() + 90.0
    found = False
    while time.monotonic() < deadline and not found:
        imap = imaplib.IMAP4_SSL(MAIL_HOST, IMAP_PORT, ssl_context=ctx)
        try:
            imap.login(ADDRESS, PASSWORD)
            imap.select("INBOX")
            # Server-side SUBJECT search can't match SEALED mail (the nest holds
            # no plaintext to index) — it returns empty even when the message is
            # present. A real MUA FETCHes and matches client-side; do the same.
            typ, ids = imap.search(None, "ALL")
            msg_ids = ids[0].split() if (typ == "OK" and ids and ids[0]) else []
            for mid in msg_ids[-25:]:
                typ, d = imap.fetch(mid, "(BODY.PEEK[HEADER.FIELDS (SUBJECT)])")
                hdr = b"".join(p[1] for p in d if isinstance(p, tuple)).decode("utf-8", "replace")
                if nonce in hdr:
                    found = True
                    break
        finally:
            try:
                imap.logout()
            except Exception:
                pass
        if not found:
            time.sleep(3.0)

    assert found, (
        f"the port-25-delivered message {nonce!r} was accepted but never became "
        f"readable over IMAP — the seal/store or MDA-serve step failed."
    )

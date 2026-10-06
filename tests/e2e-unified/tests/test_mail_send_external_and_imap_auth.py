"""tier_3: ONE client-minted credential both *sends* to an external recipient
AND *authenticates* over IMAP — the real mail-client scenario, in one flow.

The user's live scenario (2026-06-04): they configured a desktop mail client
against the deployed nest, composed mail to an external address, and the messages
sat in the **outbox, never sent**. The box log showed every submission AUTH
failing with `FfiError: General: Msg=AEAD verify failed` (and IMAP AUTH failing
the same way) — i.e. the credential the MUA presented didn't AEAD-unwrap the
client-provisioned wrapped blob. Outbound port 25 egress and inbound delivery
were both healthy; the blocker was purely **MUA AUTH**, so the mail never left
the outbox.

What no single existing test covered: `test_mail_enable_then_mua_round_trip.py`
proves submission-AUTH-then-send (to an external recipient, with DKIM verify) in
one test and IMAP-AUTH-then-read in *another*, on the SAME `default` credential
but never both surfaces in one flow. A real mail client uses **one account
credential for both SMTP and IMAP**, which is exactly where the user got stuck.
This test pins that contract end-to-end: the single credential the *client* mints
must let a normal MUA (1) AUTH the submission listener and send a message to
`test@example.com` that actually relays out, AND (2) AUTH the IMAP listener.

Production data flow asserted end-to-end on real binaries:

  the nest admin (logged into the linux app) opens mail-settings → toggles
  "Enable mail" → picks PLAIN → submits a password → `MailSettingsMachine::
  EnableMail` provisions the `default` wrapped submission token + wrapped-MSEK
  blob (both sealed under the password via Argon2id) over WS-RPC →
  a normal MUA connects to the submission listener (465, implicit TLS), does
  SASL `AUTH PLAIN` with (admin@<domain>, the client-minted password) → the
  bridge fetches the `default` wrapped submission token, AEAD-unwraps it with the
  MUA secret + verifies the inner Ed25519 signature → MAIL FROM / RCPT
  `test@example.com` → DATA → the nest DKIM-signs at the hand-out + the MTA relays via the operator-hatch
  `mta_mx_override` to the in-process stub MX (the message LEAVES the outbox) →
  the SAME MUA connects to the IMAP listener (993, implicit TLS), does SASL
  `AUTH PLAIN` with the SAME credential → the bridge AEAD-unwraps the wrapped-MSEK
  blob → `SELECT INBOX` succeeds (the mailbox is usable past auth).

Only tier_3 catches it: the wrapped submission token + wrapped-MSEK the *client*
sealed must BOTH open under the *bridge's* submission-AUTH and IMAP-AUTH paths
keyed on the same username-derived `default` credential — a client↔bridge
seal/open contract on two surfaces that no stub or in-process test exercises.

Test taxonomy:
- `tier_3` (mocking depth): every binary real, real SMTP + IMAP wire, real client
  UI minting the credential, real client-side seal + bridge-side AUTH/decrypt.
"""

import time

import pytest

from helpers.mail_wire import (
    _connect_submission_tls,
    _imap_auth_plain,
    _imap_cmd,
    _imaps_connect,
    _smtp_auth_plain,
    _wait_for_tagged,
)

# Reuse the client-driven enable-mail sibling's setup helpers (login-as-admin,
# alias the admin to a routable address, the web SPA node-url shim, the
# enable-mail gesture) rather than copy them — priority #2 (reuse, don't
# duplicate), exactly as test_mail_multi_credential_auth.py does.
from . import test_mail_enable_then_mua_round_trip as rt

# Drives the client's own "Enable mail" settings UI (PLAIN enable + the credential
# list). linux, web, and windows implement the mail-settings page (the windows
# MailSettingsPanel renders the enable toggle + PLAIN credential ids, proven
# windows-green by the dedicated_mail_nest enable→MUA round-trip coverage);
# android doesn't yet. Scope to the implementing clients so --client deselects
# it on the others instead of failing on the missing enable toggle.
pytestmark = [
    pytest.mark.tier1,
    pytest.mark.tier_3,
    pytest.mark.linux,
    pytest.mark.web,
    pytest.mark.windows,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.tui,
]

# The literal external recipient from the user's scenario. RFC 2606 reserved, and
# routed to the in-process stub MX by the dedicated_mail_nest fixture's
# [mta_mx_override], so the send is fully hermetic (no real DNS / no delivery).
_EXTERNAL_RCPT = "test@example.com"


@pytest.mark.feature("standard-mail-apps")
def test_one_credential_sends_to_external_and_imap_authenticates(
    app, dedicated_mail_nest, request
):
    """One client-minted PLAIN credential authenticates BOTH the submission
    listener (sending a message to test@example.com that relays out) AND the
    IMAP listener — the single-account mail-client contract the user's stuck
    outbox exercised."""
    handle = dedicated_mail_nest
    handle.assert_mta_running()
    nest = handle.nest
    domain = handle.domain

    # ── Client UI: log in as the nest admin and enable mail (PLAIN). The client
    # mints the `default` credential; `mua_secret` is the password a real mail
    # client (Apple Mail / Thunderbird) would store for BOTH SMTP and IMAP.
    rt._login_as_nest_admin(app, nest, rt._dedicated_node_url(app, handle, request))
    admin_addr = rt._alias_admin_to_address(nest, domain)

    app.mail_settings.navigate()
    assert app.mail_settings.is_page_visible(), "mail-settings page must be reachable"
    mua_secret = rt._client_enable_mail(app, "plain")
    assert app.mail_settings.wait_for_credential_count_at_least(1, timeout=15.0), (
        "enabling mail (PLAIN) must mint the first credential; "
        f"error: {app.mail_settings.page_error_text(timeout=2.0)!r}"
    )

    # The client's enable opened the deployment gates, so each idling bridge has
    # exited 0 for the supervisor to restart it bound (mail-bridge-lifecycle.md
    # § Default-off; internal/wsrpc/idle_gate_watch.go). The binaries e2e has no
    # s6, so play supervisor here — until this returns, no listener is bound.
    handle.rebind_after_enable()

    # ── SMTP submission: a normal MUA AUTHs on 465 with the CLIENT-MINTED secret
    # and sends to test@example.com. `_smtp_auth_plain` asserts the `235` — this
    # is the "make it past submission auth" gate that failed live with
    # `AEAD verify failed`; the `250` on `.` is the message accepted for delivery
    # (it leaves the outbox).
    nonce = f"sendexample{int(time.time() * 1000)}qx"
    message_id = f"<{nonce}@{domain}>"
    body_lines = [
        f"From: Nest Admin <{admin_addr}>",
        f"To: {_EXTERNAL_RCPT}",
        f"Subject: send-to-example.com proof {nonce}",
        f"Message-ID: {message_id}",
        "Date: Thu, 04 Jun 2026 12:00:00 +0000",
        "MIME-Version: 1.0",
        "Content-Type: text/plain; charset=utf-8",
        "",
        f"The {nonce} body must relay out to {_EXTERNAL_RCPT}.",
    ]
    raw_message = ("\r\n".join(body_lines) + "\r\n").encode()

    deadline = time.monotonic() + 40.0
    with _connect_submission_tls(handle.submission_port_465, domain) as conn:
        conn.expect("220", deadline)
        conn.cmd(f"EHLO {domain}", "250", deadline)
        # PAST SUBMISSION AUTH: AEAD-unwraps the client-sealed `default` wrapped
        # submission token with Argon2id. Raises on anything but 235.
        _smtp_auth_plain(conn, admin_addr, mua_secret, deadline)
        conn.cmd(f"MAIL FROM:<{admin_addr}>", "250", deadline)
        conn.cmd(f"RCPT TO:<{_EXTERNAL_RCPT}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(raw_message)
        conn.cmd(".", "250", deadline)
        conn.cmd("QUIT", "221", deadline)

    # The message must actually LEAVE — the bridge's OutboundWorker relays it via
    # mta_mx_override (example.com → the stub MX). This is the "actually sent"
    # the user's stuck outbox never reached.
    received = _wait_for_tagged(handle.stub_mx, nonce, timeout=20.0)
    assert received is not None, (
        f"the stub external MX received no message tagged {nonce} within 20s — "
        f"the submission to {_EXTERNAL_RCPT} did not relay out of the outbox "
        f"(see {handle.bridge_log_hint()})"
    )

    # ── IMAP: the SAME credential must make it past auth (the user's explicit
    # ask, and the surface their mail client also failed to log into). AEAD-
    # unwraps the client-sealed wrapped-MSEK blob with Argon2id as the auth
    # signal — the exact step that returned `AEAD verify failed` live.
    deadline = time.monotonic() + 40.0
    sock, buf = _imaps_connect(handle, deadline)
    with sock:
        status = _imap_auth_plain(sock, buf, "a1", admin_addr, mua_secret, deadline)
        assert status == "OK", (
            "IMAP AUTH must succeed with the SAME client-minted credential that "
            "authenticated submission — the bridge unwraps the wrapped-MSEK blob "
            f"the client sealed under it (live failure here was 'AEAD verify "
            f"failed'); got {status!r}"
        )
        # Past auth, the mailbox is usable: SELECT INBOX must succeed (proves the
        # session is genuinely authenticated, not merely a tolerated greeting).
        sel_status, sel_resp = _imap_cmd(sock, buf, "a2", "SELECT INBOX", deadline)
        assert sel_status == "OK", (
            f"SELECT INBOX after AUTH must succeed; got {sel_status}: {sel_resp!r}"
        )
        sock.sendall(b"a3 LOGOUT\r\n")

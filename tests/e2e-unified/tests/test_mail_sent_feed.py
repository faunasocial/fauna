"""tier_3: the SENT half of the client mail surface — a message the user sent
from an external SMTP-submission MUA must surface in the Fauna conversations
view as outbound mail.

A user who sends from Thunderbird / macOS Mail (real SMTP submission, NOT the
Fauna app's own compose) leaves a server-side `Sent` copy: the bridge's
submission session fires `fauna.bridges.submit_inbound_mail` for the sender's OWN
actor (`bins/fauna-bridges/internal/mta/fauna_recipient.go::deliverToFaunaActor`),
sealing the body to the sender's MSEK-derived recipient key and placing it in the
`Sent` mailbox. The client's background conversations poll then drains that feed
over `fauna.email.sent.fetch` (the Sent sibling of `inbox.fetch`), opens each
record with the SAME recipient secret as INBOX, and ingests it as an outbound
message — so the unified conversations view shows both halves of a thread
(`docs/goal/behavior/smtp-server.md` § Inbound client receive; `conversations.md`
§ Receiving).

The Sent-feed counterpart to `test_mail_client_receive.py` (the INBOX feed). It
deliberately submits to an EXTERNAL-only recipient: no local INBOX copy is
created, so the ONLY way the message can surface in conversations is the Sent
poll. A client that polls INBOX alone (every app before its Sent-feed lift)
is RED here — which is what makes this a real acceptance test for the lift, not a
tautology.

Production data flow asserted end-to-end on real binaries:

  client EnableMail provisions the sender's own recipient MLS pubkey (so the
  bridge can seal the Sent copy to it) + the wrapped submission token (so the MUA
  can AUTH) + the MSEK the client derives the recipient secret from →
  a normal MUA AUTHs on the submission listener (465, implicit TLS) with the
  client-minted PLAIN secret → DATA → the bridge seals a Sent copy to the
  sender's recipient key + `submit_inbound_mail` → nest places it in `Sent` →
  the client's conversations Sent poll (`fauna.email.sent.fetch` →
  `open_inbound_record` → parse → ingest) surfaces the decrypted subject/body in
  `list_threads`.

Test taxonomy:
- `tier_3` (mocking depth): every binary real, real SMTP submission wire, real
  client-side seal/open, real Sent-feed read.
"""

import time

import pytest

from helpers.mail_dedicated_nest import (
    alias_admin_to_address,
    dedicated_node_url,
    login_as_nest_admin,
)
from helpers.mail_wire import _connect_submission_tls, _smtp_auth_plain

# The conversations Sent poll (fauna.email.sent.fetch) is wired in shared Rust
# (NestMailInboundSource + ConversationsSession::start_receive_loop) and exercised
# by linux + web (real session under e2e by construction) and windows (the real
# session built under the `real_conversations` e2e harness — App.xaml.cs set_state
# path builds the real ConversationsSession when FAUNA_E2E_REAL_CONVERSATIONS is
# set, so the snapshot reflects real-decrypted mail instead of the mock host).
# macOS/iOS join here (2026-07-15; same real-`ConversationsSession`-under-
# FAUNA_E2E_REAL_CONVERSATIONS pattern, conftest `_apply_real_conversations_env`
# is already client-agnostic). android joined too — its own
# `ConversationsManagerHost.startConversationsSession` wires the same real
# session under `FAUNA_E2E_REAL_CONVERSATIONS` (landed 2026-07-20); this stale
# note (2026-06-05) predates that landing, and its own e2e run stays
# emulator-host-gated like every android track . Mark the
# supported clients so --client deselects this elsewhere rather than building
# the bridge and then skipping in-body (the build is session-scoped + slow).
# `real_conversations` drives the windows real-session launch flag (see
# pytest.ini + _build_app_config).
pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.linux,
    pytest.mark.web,
    pytest.mark.windows,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.tui,
    pytest.mark.android,
    pytest.mark.real_conversations,
]

# The PLAIN credential the client mints and the submission MUA authenticates with.
_MUA_PASSWORD = "sent-feed-plain-pw-1"


@pytest.mark.feature("email-in-conversations")
def test_external_mua_submission_surfaces_in_conversations_sent(
    app, dedicated_mail_nest, request
):
    """Enable mail in the client (as the nest admin), submit an outbound message
    through the SMTP submission listener (the external-MUA path), and prove the
    server-side Sent copy surfaces in the conversations view via the Sent feed."""
    handle = dedicated_mail_nest
    handle.assert_mta_running()
    nest = handle.nest
    domain = handle.domain

    # ── 1. Log the client in as the nest admin and give it a routable address.
    login_as_nest_admin(app, nest, dedicated_node_url(app, handle, request))
    admin_addr = alias_admin_to_address(nest, domain)

    # ── 2. Enable mail through the client UI (PLAIN). This provisions the
    # sender's own recipient MLS pubkey (so the bridge can seal the Sent copy to
    # it), the wrapped submission token (so the MUA can AUTH), and the MSEK the
    # client derives the recipient secret from to OPEN the Sent copy client-side.
    app.mail_settings.navigate()
    assert app.mail_settings.is_page_visible(), "mail-settings page must be reachable"
    app.mail_settings.enable_mail_plain(_MUA_PASSWORD)
    assert app.mail_settings.wait_for_credential_count_at_least(1, timeout=15.0), (
        "enabling mail must mint the first credential; "
        f"error: {app.mail_settings.page_error_text(timeout=2.0)!r}"
    )
    assert app.mail_settings.wait_for_enabled_status(timeout=15.0), (
        "mail must report enabled after the toggle; "
        f"status={app.mail_settings.status_text()!r}, "
        f"error={app.mail_settings.page_error_text(timeout=2.0)!r}"
    )

    # The client's enable opened the deployment gates, so each idling bridge has
    # exited 0 for the supervisor to restart it bound (mail-bridge-lifecycle.md
    # § Default-off; internal/wsrpc/idle_gate_watch.go). The binaries e2e has no
    # s6, so play supervisor here — until this returns, no listener is bound.
    handle.rebind_after_enable()

    # ── 3. A normal MUA submits an outbound message to an EXTERNAL recipient via
    # the submission listener (465), authenticating with the client-minted PLAIN
    # secret. External-only → no local INBOX copy is created; the bridge fires
    # exactly one `submit_inbound_mail` for the sender's own actor (the Sent
    # copy). The unique nonce makes the conversations assertion unambiguous.
    external_rcpt = "recipient@external.test"
    nonce = f"sentfeed{int(time.time() * 1000)}qx"
    message_id = f"<{nonce}@{domain}>"
    body_lines = [
        f"From: Nest Admin <{admin_addr}>",
        f"To: {external_rcpt}",
        f"Subject: Sent-feed proof {nonce}",
        f"Message-ID: {message_id}",
        "Date: Mon, 25 May 2026 12:30:00 +0000",
        "MIME-Version: 1.0",
        "Content-Type: text/plain; charset=utf-8",
        "",
        f"The {nonce} body must surface in the conversations Sent feed.",
    ]
    raw_message = ("\r\n".join(body_lines) + "\r\n").encode()

    deadline = time.monotonic() + 40.0
    with _connect_submission_tls(handle.submission_port_465, domain) as conn:
        conn.expect("220", deadline)
        conn.cmd(f"EHLO {domain}", "250", deadline)
        _smtp_auth_plain(conn, admin_addr, _MUA_PASSWORD, deadline)
        conn.cmd(f"MAIL FROM:<{admin_addr}>", "250", deadline)
        conn.cmd(f"RCPT TO:<{external_rcpt}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(raw_message)
        conn.cmd(".", "250", deadline)
        conn.cmd("QUIT", "221", deadline)

    # ── 4. The client's background conversations poll drains the Sent feed
    # (fauna.email.sent.fetch) on its own cadence (2 s in e2e), opens each record
    # with the recipient secret, and ingests it. Poll `list_threads` until the
    # DECRYPTED message surfaces — its plaintext subject/body carry the nonce, so
    # its appearance proves the Sent-feed open succeeded.
    found = None
    poll_deadline = time.monotonic() + 60.0
    while time.monotonic() < poll_deadline and found is None:
        for t in app.conversations.list_threads():
            if nonce in (t.label or "") or nonce in (t.snippet or ""):
                found = t
                break
        if found is None:
            time.sleep(1.0)

    threads_dump = [(t.label, t.snippet, t.rail) for t in app.conversations.list_threads()]
    assert found is not None, (
        f"the externally-submitted message tagged {nonce!r} never surfaced in the "
        f"conversations view within 60s — the client Sent-feed path "
        f"(sent.fetch → open_inbound_record → parse → ingest) did not complete. "
        f"A client that polls only INBOX (no Sent feed) fails here.\n"
        f"  threads: {threads_dump}\n"
        f"  conversations error: {app.error_text()!r}\n"
        f"  {handle.bridge_log_hint()}"
    )
    # Sent mail lands on the same Smtp rail as received mail (one unified thread).
    assert found.rail == "Smtp", (
        f"the Sent copy must land on the Smtp rail; got {found.rail!r}"
    )

"""tier_3: the FIRST-PARTY Sent-copy DURABILITY path, proven through a real WEB
app STORE-CLEAR + RE-HYDRATE (the web variant of the linux process-restart proof
in `test_mail_sent_copy_restart.py`).

History (2026-06-13): this test was briefly committed `xfail` because the
web compose→send appeared not to land at the nest — the echo rendered on the **Smtp**
rail with no error, yet no durable Sent copy was written and no outbound enqueued. Root
cause was a **test-harness** bug, NOT a product bug: the `accept_recipient_chip` action
falls back to the `conversations_accept_recipient` bridge command when the recipient
picker shows no clickable suggestion (always the case for a plain external email), and
that command called `ensureManagerForTest()` → `installMockBackendsForTest()`, which
replaced the real `SmtpBackend`/`FaunaMls` rails with no-op `MockRailBackend`s. The
subsequent `dm-send-button` then resolved the **mock** Smtp backend, which returns a
synthetic `Ok(SendOutcome)` with no wire I/O — so the send was silently swallowed (echo
only). Fix: `conversations_accept_recipient` now uses the REAL manager
(`getConversationsManager()`), like the `conversations_real_*` commands. The real
`SmtpBackend → WasmSmtpSink → fauna.email.send` path was always correct (the singleton
`emailSend` proved it), so production was never affected.

Why a SEPARATE web test (not a skip-branch in the linux file): web drives a DIFFERENT
send+receive path than the four native apps. The natives ride the shared
`ConversationsSession::start_receive_loop`; web has no such loop — it drives its own
JS poll (`$lib/conversations.ts::startReceivePoll` → `pollOnce` → `emailSentFetch`)
over a wasm `ConversationsManager`. A web restart regression guard is therefore NOT
redundant with the linux one.

Why the store-clear is load-bearing (the "teeth"): the in-session local echo MASKS the
server copy. The faithful web clear is a full page reload (`PlatformDriver.hard_reload()`
→ `location.reload()`): it tears down the entire JS VM, so the wasm `ConversationsManager`
module singleton, its snapshot store, AND the per-feed poll cursors (`afterUid` /
`afterUidSent`, which reset to uid 0) are all destroyed. The post-reload view can
therefore ONLY be rebuilt by the receive poll re-draining `fauna.email.sent.fetch` from
the server. `location.reload()` keeps the browser's origin storage and the SPA persists
the identity to `localStorage` (`fauna_secret` / `fauna_node_url`), so the reloaded SPA
re-hydrates the same actor without re-login.

Why an EXTERNAL recipient: an external-only send leaves no local INBOX copy, so the ONLY
way the message can surface after the clear is the Sent feed (same rationale as
`test_mail_sent_feed.py`).

Production data flow asserted end-to-end on real binaries:

  client EnableMail provisions the sender's own recipient MLS pubkey + the MSEK the
  client derives the recipient secret from → the client composes a new conversation and
  clicks Send (→ `fauna.email.send`) → the nest `send_handler` enqueues the outbound row
  AND seals a Sent copy to the sender's read key → the message is visible (local echo) →
  **the page is fully reloaded (empty wasm store, cursors reset to uid 0)** → the SPA
  re-hydrates the SAME actor from `localStorage` and re-derives the MSEK → the JS receive
  poll drains `fauna.email.sent.fetch`, opens the sealed copy, ingests it → the sent
  message is STILL visible in its thread.

Test taxonomy:
- `tier_3` (mocking depth): every binary real, real `fauna.email.send` wire, real
  client-side seal/open, real Sent-feed read across a real page reload.
- WEB only: the faithful clear is a `location.reload()` (`hard_reload()`); the native
  process-restart variants are covered (linux) or are the cross-area follow-on.
"""

import time

import pytest

from helpers.app_surface import declared_absence
from helpers.mail_dedicated_nest import (
    alias_admin_to_address,
    dedicated_node_url,
    login_as_nest_admin,
)

# tier_3 + web-only. xfail(strict): the web compose→send currently does not land at the
# nest (see module docstring); when that is fixed this test xpasses
# and strict xfail forces removal of the marker.
pytestmark = [pytest.mark.tier_3, pytest.mark.web]

# The PLAIN credential the client mints when enabling mail.
_MUA_PASSWORD = "sent-copy-restart-web-plain-pw-1"

# Routed to the dedicated nest's stub external MX (conftest `dedicated_mail_nest`), so a
# first-party send relays out hermetically and the nest writes the durable Sent copy.
# External-only → no local INBOX copy → the Sent feed is the only post-reload path.
_EXTERNAL_RECIPIENT = "recipient@external.test"


def _thread_with_nonce(app, nonce: str):
    """First conversations thread whose label/snippet carries `nonce`, or None."""
    for t in app.conversations.list_threads():
        if nonce in (t.label or "") or nonce in (t.snippet or ""):
            return t
    return None


def _poll_for_nonce(app, nonce: str, timeout: float):
    deadline = time.monotonic() + timeout
    found = None
    while time.monotonic() < deadline and found is None:
        found = _thread_with_nonce(app, nonce)
        if found is None:
            time.sleep(1.0)
    return found


@pytest.mark.feature("email-in-conversations")
def test_first_party_sent_copy_survives_web_reload(app, dedicated_mail_nest, request):
    """Compose+send through the web app, reload the page (clearing the in-memory
    store), and prove the sent message is STILL visible — reloaded purely from the
    server-side Sent copy."""
    if not app.driver.is_web():
        declared_absence(
            app.driver,
            capability="the web page-reload (hard_reload() → "
            "location.reload()) Sent-copy restart proof",
            doc="testing.md § Cross-app e2e conventions, point 7 (native "
            "apps prove the identical guarantee via their own process-"
            "relaunch twin, test_mail_sent_copy_restart.py)",
        )

    handle = dedicated_mail_nest
    handle.assert_mta_running()
    nest = handle.nest
    domain = handle.domain
    node_url = dedicated_node_url(app, handle, request)

    # ── 1. Log the client in as the nest admin and give it a routable address, then
    # enable mail (PLAIN): provisions the sender's recipient MLS pubkey + the MSEK the
    # client derives the recipient secret from (to seal+open the Sent copy client-side).
    login_as_nest_admin(app, nest, node_url)
    admin_addr = alias_admin_to_address(nest, domain)

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

    # ── 2. Compose + send a first-party message through the conversations UI. The send
    # routes through wasm ConversationsManager::send_new_thread → SmtpBackend::send →
    # fauna.email.send (WS-RPC), and the nest's send_handler seals a durable Sent copy.
    nonce = f"sentcopyrestartweb{int(time.time() * 1000)}qx"
    subject = f"Sent-copy web-reload proof {nonce}"
    body = f"The {nonce} body must survive a web page reload via the server Sent copy.\n"
    app.conversations.start_new_conversation(
        _EXTERNAL_RECIPIENT, subject=subject, body=body
    )

    # ── 3. PRE-RELOAD: the sent message is visible in the conversations view (the
    # in-session local echo). This is the baseline the reload must preserve — NOT the
    # assertion under test (the echo would satisfy it).
    pre = _poll_for_nonce(app, nonce, timeout=40.0)
    pre_dump = [(t.label, t.snippet, t.rail) for t in app.conversations.list_threads()]
    assert pre is not None, (
        f"the just-sent message tagged {nonce!r} never appeared in the conversations view "
        f"before the reload — fauna.email.send did not surface client-side.\n"
        f"  threads: {pre_dump}\n"
        f"  conversations error: {app.error_text()!r}"
    )

    # ── 4. RELOAD the page: location.reload() tears down the entire JS VM, so the wasm
    # ConversationsManager singleton, its snapshot store, AND the per-feed poll cursors
    # (afterUid/afterUidSent reset to uid 0) are all destroyed. localStorage (identity +
    # node_url) survives, so the SPA re-hydrates the SAME actor without re-login.
    app.driver.hard_reload()

    # ── 5. Re-hydrate the mail config so the recipient secret is re-derivable.
    # ensure_mail_enabled() is idempotent (mail is already enabled server-side).
    app.mail_settings.navigate()
    app.mail_settings.ensure_mail_enabled()

    # ── 6. POST-RELOAD ASSERTION: the sent message is STILL visible — reloaded purely
    # from the server-side Sent copy via the poll's fauna.email.sent.fetch drain (from
    # uid 0). XFAIL today: the compose-send above never created the server Sent copy.
    app.conversations.navigate()
    post = _poll_for_nonce(app, nonce, timeout=60.0)
    post_dump = [(t.label, t.snippet, t.rail) for t in app.conversations.list_threads()]
    assert post is not None, (
        f"the sent message tagged {nonce!r} did NOT survive the web page reload — it was "
        f"not reloaded from the server-side Sent copy within 60s (the web compose→send "
        f"did not produce a durable server Sent copy).\n"
        f"  threads: {post_dump}\n"
        f"  conversations error: {app.error_text()!r}\n"
        f"  {handle.bridge_log_hint()}"
    )
    # Sent mail lands on the same Smtp rail as received mail (one unified thread).
    assert post.rail == "Smtp", (
        f"the reloaded Sent copy must land on the Smtp rail; got {post.rail!r}"
    )

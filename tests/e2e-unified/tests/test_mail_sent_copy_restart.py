"""tier_3: the FIRST-PARTY Sent-copy DURABILITY path, proven through a real client
PROCESS RESTART.

The "sent mail vanishes on restart" bug: a message composed in a Fauna app used to
be echoed only client-side (in the in-memory `ConversationsManager` store). On the next
launch the conversation view rebuilt itself purely from the server read-feeds — and since
no server-side copy of a first-party `fauna.email.send` existed, the sent message
disappeared. The durability fix (an internal follow-up track) made `send_handler` write a
durable server-side **Sent copy** via `seal_and_store_sent_copy`
(`bins/fauna-nest/src/email_handlers.rs:719` → `bridge_routing_handlers.rs:3380`), sealed
to the sender's own MSEK-derived read key, reloaded via `fauna.email.sent.fetch`
(`sent_fetch_handler`, `email_handlers.rs:878`).

The nest + shared-Rust layers prove the durability claim DETERMINISTICALLY
(`conformance_email_send_in_domain.rs::in_domain_send_lands_in_recipient_sealed_inbox`;
`smtp_backend_tests.rs::ingest_into_empty_store_reloads_the_sent_copy`). This e2e covers
the leg those CANNOT: the live LINUX APP, after an actual process restart, re-acquiring
its read key and rebuilding the conversation store *purely from the server* so the sent
message is still there. It is the GUI client-process-restart acceptance the goal doc
entrusts to the linux manual test
(`docs/goal/behavior/smtp-server.md` § Implementation status today, line 34).

Why the restart is load-bearing: the in-session local echo MASKS the server copy. Without
clearing the in-memory store, "still visible" is a tautology (the echo never left). The
faithful clear is a real client process restart — `PlatformDriver.recover()` (teardown +
relaunch a fresh process; `tests/e2e-unified/drivers/linux.py:198`) gives an empty store +
a fresh data dir, so the post-restart view can ONLY come from the server Sent copy.

Why an EXTERNAL recipient: an external-only send leaves no local INBOX copy, so the ONLY
way the message can surface after the restart is the Sent feed (the same rationale as
`test_mail_sent_feed.py`). A client that wrote no durable Sent copy — or one that fails to
re-acquire the MSEK after a restart — is RED here.

Production data flow asserted end-to-end on real binaries:

  client EnableMail provisions the sender's own recipient MLS pubkey + the MSEK the client
  derives the recipient secret from → the client composes a new conversation and clicks Send
  (`dm-send-button` → `ConversationsManager::send_new_thread` → `SmtpBackend::send` →
  `fauna.email.send`) → the nest `send_handler` enqueues the outbound row AND seals a Sent
  copy to the sender's read key in the `Sent` mailbox → the message is visible in the
  client's conversations view (local echo) → **the client process is restarted (fresh
  process, empty store, fresh data dir)** → the client re-logs-in as the SAME actor and
  re-acquires the MSEK → the shared receive loop drains `fauna.email.sent.fetch`, opens the
  sealed copy with the re-derived recipient secret, ingests it → the sent message is STILL
  visible in its thread.

Test taxonomy:
- `tier_3` (mocking depth): every binary real, real `fauna.email.send` wire, real
  client-side seal/open, real Sent-feed read across a real process restart.
- LINUX + WINDOWS: the faithful restart is a native-driver process relaunch (`recover()`);
  the web driver's `recover()` does not relaunch the browser, and macos/ios/android are the
  cross-area follow-on (the goal doc's per-app Gap). The windows leg depends on the FlaUI
  bridge's per-session epoch fence (`flaui-bridge/Program.cs` + `TestAgent` echoing
  `FAUNA_E2E_SESSION_EPOCH` on /app/commands+/app/state): under E2E single-instancing is
  disabled, so the pre-`recover()` FaunaApp process can briefly outlive the relaunch and its
  agent keep polling the bridge's process-global command queue — without the epoch fence it
  steals the post-restart commands and mounts UI in the dead window while FlaUI inspects the
  new one (flaky count=0). The fence makes the stale agent a no-op.
"""

import time

import pytest

from helpers.app_surface import declared_absence, skip_unbuilt
from helpers.mail_dedicated_nest import (
    alias_admin_to_address,
    dedicated_node_url,
    login_as_nest_admin,
)
from i18n.strings import S

# The first-party Sent-copy durability path is exercised here through a real client
# process restart (recover()). Scoped to the native-relaunch clients (linux + windows +
# macOS) so --client deselects it elsewhere rather than building the mail bridge and
# skipping in-body (the build is session-scoped + slow). `real_conversations` makes the
# windows/macOS app launch the REAL ConversationsSession receive loop under e2e
# (FAUNA_E2E_REAL_CONVERSATIONS) so the post-restart Sent copy drains via
# fauna.email.sent.fetch — see conftest._build_app_config. macOS's `recover()`
# (InProcessAgentDriver: teardown + relaunch) already carries a durable e2e Keychain
# (FAUNA_E2E_CREDENTIAL_DIR) across the restart; iOS's twin is not yet built
# (tracked internally under "iOS crash-recovery journeys" — a separate track) so iOS stays out here.
pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.linux,
    pytest.mark.windows,
    pytest.mark.macos,
    pytest.mark.tui,
    pytest.mark.real_conversations,
]

# The PLAIN credential the client mints when enabling mail.
_MUA_PASSWORD = "sent-copy-restart-plain-pw-1"

# Routed to the dedicated nest's stub external MX (conftest `dedicated_mail_nest`
# op_hatch_extra), so the first-party send relays out hermetically and the nest writes the
# durable Sent copy. External-only → no local INBOX copy → the Sent feed is the only path
# the message can surface after the restart.
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
def test_first_party_sent_copy_survives_client_restart(app, dedicated_mail_nest, request):
    """Compose+send through the Fauna app, restart the client process, and prove the
    sent message is STILL visible — reloaded purely from the server-side Sent copy."""
    if app.driver.is_web():
        declared_absence(
            app.driver,
            capability="a native-process-relaunch Sent-copy restart proof",
            doc="testing.md § Cross-app e2e conventions, point 7 (web's "
            "recover() does not relaunch the browser; its own reload-based "
            "twin is test_mail_sent_copy_restart_web.py)",
        )
    elif not (
        app.driver.is_linux()
        or app.driver.is_windows()
        or app.driver.is_macos()
        or app.driver.is_tui()
    ):
        skip_unbuilt(
            app.driver,
            surface="a native-process-relaunch Sent-copy restart proof",
            detail="linux + windows + macOS + tui have it (PlatformDriver."
            "recover() does a real teardown+relaunch); iOS + android are "
            "the cross-app follow-on",
            tracked="docs/goal/behavior/smtp-server.md § Implementation "
            "status today",
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

    # ── 2. Compose + send a first-party message through the conversations UI. The
    # send routes through ConversationsManager::send_new_thread → SmtpBackend::send →
    # fauna.email.send (WS-RPC), and the nest's send_handler seals a durable Sent copy
    # (seal_and_store_sent_copy) in addition to enqueuing the outbound relay. The unique
    # nonce makes the conversations assertion unambiguous.
    nonce = f"sentcopyrestart{int(time.time() * 1000)}qx"
    subject = f"Sent-copy restart proof {nonce}"
    body = f"The {nonce} body must survive a client restart via the server Sent copy.\n"
    app.conversations.start_new_conversation(
        _EXTERNAL_RECIPIENT, subject=subject, body=body
    )

    # ── 3. PRE-RESTART: the sent message is visible in the conversations view (the
    # in-session local echo and/or the freshly-written Sent copy). This is the baseline
    # the restart must preserve — NOT the assertion under test (the echo would satisfy it).
    pre = _poll_for_nonce(app, nonce, timeout=40.0)
    pre_dump = [(t.label, t.snippet, t.rail) for t in app.conversations.list_threads()]
    assert pre is not None, (
        f"the just-sent message tagged {nonce!r} never appeared in the conversations view "
        f"before the restart — fauna.email.send did not surface client-side.\n"
        f"  threads: {pre_dump}\n"
        f"  conversations error: {app.error_text()!r}"
    )

    # ── 3b. The thread being VISIBLE does not mean the send SUCCEEDED. A send the
    # backend refuses locally still leaves a thread carrying this nonce in its label
    # (SendState::Failed renders, it does not vanish), so the assertion above passes
    # identically on a refused send — and the failure then only surfaces 60s later at
    # step 6 as an empty post-restart view, which reads like a Sent-copy/MSEK bug
    # anywhere in nest or shared Rust. That blindness cost real sessions: the macOS
    # leg regressed on 2026-08-02 (a change hardened SmtpBackend's sender
    # pre-check from `!contains('@')` to `SelfAddress::usable`, which correctly began
    # refusing the `"@<nest-url-host>"` shape apple's e2e session-build composes when
    # the session patch carries no handle) and presented as exactly this test's
    # post-restart failure, with the nest proven correct in isolation.
    #
    # A local refusal renders on the app's own `error-message`
    # (`conversations.md` § Errors & edge cases: always `conversations.unified.error_send`
    # with the backend's rejection as `{message}`; conventions 2 + 11) — so an error
    # sitting here means the send never reached the wire and there is no server-side
    # Sent copy for the restart to reload. Read ONCE, with no wait: both the thread
    # render and the refusal are effects of the same `send_new_thread` call, and this
    # check is deliberately one-directional — a not-yet-rendered error simply falls
    # through to the existing post-restart assertion, exactly as before. It can turn
    # a silent false-green into a diagnosis; it can never invent a red.
    send_error = app.error_text()
    assert not send_error, (
        f"the send of {nonce!r} was REFUSED locally — no fauna.email.send reached the "
        f"nest, so no durable Sent copy exists and the restart proof below cannot "
        f"pass for reasons that have nothing to do with the Sent-copy path.\n"
        f"  error-message: {send_error!r}\n"
        f"  expected for an unresolved self-address: "
        f"{S.conversations.unified.error_send(message=S.error.email.no_handle)!r}\n"
        f"  (that shape means this client composed a self-address with an empty local "
        f"part — it must call ConversationsSession.set_self_address with the resolved "
        f"<handle>@<domain> once identity lands; conversations.md § Self-address: live, "
        f"never baked)\n"
        f"  threads: {pre_dump}"
    )

    # ── 4. RESTART the client process: teardown + relaunch a FRESH process (empty
    # in-memory ConversationsManager store + fresh data dir). This is the load-bearing
    # step — it removes the in-session echo so the post-restart view can ONLY come from
    # the server. The relaunch reuses the cached launch config (same FAUNA_CONV_POLL_SECS),
    # and kills only this driver's own tracked PID (no name-based pkill).
    assert app.driver.recover(), (
        "client process relaunch (driver.recover()) failed — cannot run the restart proof"
    )

    # ── 5. Re-login as the SAME actor and re-acquire the MSEK. The fresh data dir holds
    # no identity and no cached mail key, so the client must re-seed the admin secret and
    # re-fetch the mail config (incl. the wrapped MSEK) from the server before it can OPEN
    # the sealed Sent copy. ensure_mail_enabled() is idempotent: mail is already enabled
    # server-side, so it confirms the credential and re-hydrates the mail config.
    login_as_nest_admin(app, nest, node_url)
    app.mail_settings.navigate()
    app.mail_settings.ensure_mail_enabled()

    # ── 6. POST-RESTART ASSERTION: the sent message is STILL visible — reloaded purely
    # from the server-side Sent copy via the receive loop's fauna.email.sent.fetch drain.
    # A client that wrote no durable Sent copy (pre-fix) OR fails to re-acquire the MSEK
    # after a restart is RED here.
    app.conversations.navigate()
    post = _poll_for_nonce(app, nonce, timeout=60.0)
    post_dump = [(t.label, t.snippet, t.rail) for t in app.conversations.list_threads()]
    assert post is not None, (
        f"the sent message tagged {nonce!r} did NOT survive the client restart — it was "
        f"not reloaded from the server-side Sent copy (fauna.email.sent.fetch → open with "
        f"the re-acquired recipient secret → ingest) within 60s. Either the durable Sent "
        f"copy was not written, or the client did not re-acquire the MSEK after the "
        f"restart.\n"
        f"  threads: {post_dump}\n"
        f"  conversations error: {app.error_text()!r}\n"
        f"  {handle.bridge_log_hint()}"
    )
    # Sent mail lands on the same Smtp rail as received mail (one unified thread).
    assert post.rail == "Smtp", (
        f"the reloaded Sent copy must land on the Smtp rail; got {post.rail!r}"
    )

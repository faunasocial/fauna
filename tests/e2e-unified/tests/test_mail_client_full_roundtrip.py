"""tier_3: the FULL single-user mail round-trip, configured entirely through the
linux app UI, starting from a fresh UNCLAIMED nest — across both credential
kinds.

This is the out-of-the-box mail experience proof (tracked internally,
Slice 2; user directive 2026-05-26): a user installs a fresh nest + the client,
**claims the nest through the client** (becoming admin) with a handle that
carries its domain (`alice@fauna.test`), and that **handle IS their email
address** — they then **enable mail through the client** and **send + receive**
real mail, with every configuration step driven through the client UI:

  1. claim the unclaimed nest (import identity → handle `alice@fauna.test` →
     connect by URL → one-time claim code → dismiss the terminal
     nat_mode_choice step);
  2. the handle's domain (`fauna.test`) is **auto-registered** as the mail domain
     at claim time;
  3. the claim's derived mail enable mints the admin's mailbox (which makes
     `alice@fauna.test` a routable address — the canonical-handle-alias product
     behavior, Slice 1) and auto-approves the box's own mail bridge, so there is
     no approval card to click (`mail-bridge-lifecycle.md` § Onboarding
     auto-approval);
  4. add a credential of the parametrized kind on the mail-settings page;
  5. compose + send to an external recipient through the conversations UI;
  6. receive an external reply, rendered decrypted in the conversations list.

Steps 1–4 (the claim + provision + credential UI dance) live in the shared
`helpers.mail_client_ui.claim_enable_and_ready` so the auto-reply round-trip
test (`test_mail_client_reply_roundtrip.py`) reuses the exact same flow; this
test layers the send (5) + hand-crafted external reply (6) on top, parametrized
over the **credential kind** added to the claim's mailbox: `oauthbearer` vs
`plain` (the password an external Thunderbird-style MUA would AUTH with).
Native Fauna apps send/receive over WS-RPC (not as MUAs), so the kind
doesn't change the client round-trip — but adding either must leave a working
mailbox.

No-modes retirement (ratified 2026-07-12): this file used to also parametrize
over the admin's storage-mode commit at claim (`plaintext` vs `encrypted`),
asserting that mail — HPKE-sealed to the recipient either way — was
transparent to the storage mode. That axis is RETIRED along with the
onboarding storage-mode question itself: every nest is sealed at rest
unconditionally now, so there is no longer a second arm to be transparent
across. Collapsed from 4 cells (storage × credential) to 2 (credential only).

Accepted minimal scaffolding ("cheating"), because you cannot get real DNS / a
real CA cert / a real external mail server for a test domain: the in-process
`stub_mx` external MX reached via the bridge's `mta_mx_override`, fake
clamd/rspamd, the bridge's sealed TLS-cert blob, and a `put_spam_policy` that
clears the DNS-perimeter gates. Everything a real admin/user touches is the UI.

Test taxonomy: tier_3 (every binary real, real SMTP wire, real seal → client
fetch → client open → parse → render).
"""

import time

import pytest

from helpers.app_surface import skip_unbuilt
from helpers.mail_client_ui import claim_enable_and_ready, deliver_inbound

pytestmark = pytest.mark.tier_3

# The (external) sender/recipient domain — routed to the stub MX by the bridge's
# mta_mx_override, and (for inbound) with no loopback/local-domain exemption so a
# reply arrives as genuine external mail.
EXTERNAL_DOMAIN = "external.test"


def _run_full_roundtrip(app, nest, *, credential_kind):
    """The shared base method: a complete UI-driven single-user mail round-trip
    against a fresh unclaimed nest, parametrized over credential kind. See the
    module docstring for the staged flow."""
    nonce = f"uiroundtrip{int(time.time() * 1000)}qx"

    # ── 1–5. Claim → provision → bridge serving → add credential → conversations.
    ctx = claim_enable_and_ready(app, nest, credential_kind=credential_kind)
    domain = ctx["domain"]
    handle = ctx["handle"]

    # ── 6. Send to an external recipient through the conversations UI ───────
    recipient = f"bob@{EXTERNAL_DOMAIN}"
    subject = f"UI round-trip out {nonce}"
    app.conversations.start_new_conversation(
        recipient, subject=subject, body=f"Outbound {nonce} from the Fauna app.\n"
    )

    # Reception verified IN THE STUB MX (the external 'fake gmail' got it).
    deadline = time.monotonic() + 90.0
    relayed = None
    while time.monotonic() < deadline and relayed is None:
        for raw in nest.stub_mx.messages():
            if nonce.encode() in raw:
                relayed = raw
                break
        if relayed is None:
            time.sleep(0.5)
    # Whether the client recorded the sent thread — proves `fauna.email.send`
    # fired (vs. a UI/enqueue miss), to disambiguate a relay-only failure.
    _threads = [(t.label, t.snippet, t.rail) for t in app.conversations.list_threads()]
    _sent_seen = any(nonce in (t[0] or "") or nonce in (t[1] or "") for t in _threads)
    assert relayed is not None, (
        f"the stub external MX never received the client-sent message {nonce!r}. "
        f"sent-thread-recorded-in-client={_sent_seen}; threads={_threads}; "
        f"conversations error: {app.error_text()!r}; {nest.bridge_log_hint()}"
    )
    assert recipient.encode() in relayed, "relayed message must carry the recipient"

    # Send also SEEN IN THE CLIENT: the composed thread is in the list.
    assert _sent_seen, (
        f"the sent message {nonce!r} is not visible in the client's conversations "
        f"list; threads: {_threads}"
    )

    # ── 7. Receive an external reply, rendered decrypted in the client ──────
    reply_nonce = f"uireply{int(time.time() * 1000)}qx"
    reply = (
        "\r\n".join([
            f"From: Bob <bob@{EXTERNAL_DOMAIN}>",
            f"To: {handle}",
            f"Subject: Re: {subject} {reply_nonce}",
            f"Message-ID: <{reply_nonce}@{EXTERNAL_DOMAIN}>",
            "Date: Mon, 26 May 2026 12:00:00 +0000",
            "MIME-Version: 1.0",
            "Content-Type: text/plain; charset=utf-8",
            "",
            f"Reply body carrying {reply_nonce}.",
        ]) + "\r\n"
    ).encode()
    deliver_inbound(
        nest.mx_port, domain, f"reply@{EXTERNAL_DOMAIN}", handle, reply,
        time.monotonic() + 40.0,
    )

    # Receive SEEN IN THE CLIENT: the decrypted reply surfaces in the list
    # (via the fauna.mail.received arrival push or the periodic poll backstop).
    deadline = time.monotonic() + 60.0
    found = None
    while time.monotonic() < deadline and found is None:
        for t in app.conversations.list_threads():
            if reply_nonce in (t.label or "") or reply_nonce in (t.snippet or ""):
                found = t
                break
        if found is None:
            time.sleep(1.0)
    assert found is not None, (
        f"the external reply {reply_nonce!r} never surfaced decrypted in the "
        f"conversations list — the client receive path (inbox.fetch → "
        f"open_inbound_record → parse → render) did not complete.\n"
        f"  threads: {[(t.label, t.snippet, t.rail) for t in app.conversations.list_threads()]}\n"
        f"  conversations error: {app.error_text()!r}; {nest.bridge_log_hint()}"
    )
    assert found.rail == "Smtp", f"received mail must land on the Smtp rail; got {found.rail!r}"


# Every credential-kind must round-trip. Each parametrization gets a fresh
# unclaimed nest + MTA bridge (function-scoped fixture), so the two runs are
# fully independent.
@pytest.mark.parametrize("credential_kind", ["oauthbearer", "plain"])
@pytest.mark.feature("email-in-conversations")
def test_full_client_ui_mail_roundtrip(app, unclaimed_mail_nest_ui, credential_kind):
    if not (app.driver.is_linux() or app.driver.is_macos() or app.driver.is_ios()):
        skip_unbuilt(
            app.driver,
            surface="the full client-UI mail round-trip",
            detail="wired on linux first; web/windows/android/tui are "
            "the cross-app follow-on",
            tracked="mail-settings.md",
        )
    _run_full_roundtrip(app, unclaimed_mail_nest_ui, credential_kind=credential_kind)

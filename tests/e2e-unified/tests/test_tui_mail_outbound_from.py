"""tier_3: the tui app's outbound mail ``From:`` is ``<handle>@<handle-domain>``.

Item 5(b) of the tui mail milestone. Before this, tui left the SMTP rail's
``self_address`` empty unless the logged-in handle literally carried ``@domain``,
so a client-composed mail went out with an empty/absent ``From:`` — silently
non-replyable (a human hitting "Reply" has no address, and a real box bounces
``550 Relay access denied``). The fix resolves ``<handle>@<domain>`` from the
shared account registry — the whoami/verify HANDLE domain, populated by a
background silent challenge (``session::spawn_domain_refresh``) — and lazily
re-registers the SMTP backend on compose, mirroring linux's ``ensure_smtp_backend``
(``client-outbound-from-uses-handle-domain``).

This is the SEND counterpart to ``test_mail_client_reply_roundtrip.py`` (which
proves replyability on linux through the admin-claim UI tui lacks): here the nest
is started with ``handle_domain == the mail domain``
(``dedicated_mail_nest_handle_domain``), the admin enables mail through the real
mail-settings UI, composes to an external recipient, and the stub external MX
captures the relayed message — whose ``From:`` must be ``<handle>@fauna.test`` (a
routable address in the local mail domain), not the empty header the gap produced.

Why this isolates the fix cleanly: the SMTP *envelope* sender is the authenticated
user, so the message RELAYS to the stub regardless of the header ``From:`` — the
"relayed" gate (step 5) passes with OR without the fix (proving the harness
works), and only the ``From:`` assertion (step 6) is red before the fix.

tui-only for now (both halves of mail item 5 are the tui milestone's); the
replyability property generalizes to the other apps via
``test_mail_client_reply_roundtrip.py``.

Test taxonomy: tier_3 — every binary real, real SMTP wire out to the stub MX,
real client-side seal + submission over ``fauna.email.send``.
"""

import email.utils
import time

import pytest

from helpers.e2e_session import E2E_LOGIN_DEVICE_ID
from helpers.mail_wire import header_value
from helpers.mail_aliases import add_exact_alias

pytestmark = [pytest.mark.tier_3, pytest.mark.tui]

# Routed to the stub external MX by the fixture's `mta_mx_override`, so the send
# is fully hermetic (no real DNS / no real delivery).
EXTERNAL_DOMAIN = "external.test"
# The handle localpart the client logs in with; aliased to the admin actor below
# so `<localpart>@<domain>` is a routable local mailbox (replyability).
HANDLE_LOCAL = "admin"


@pytest.mark.feature("email-in-conversations")
def test_tui_outbound_from_is_handle_at_handle_domain(
    app, dedicated_mail_nest_handle_domain
):
    handle = dedicated_mail_nest_handle_domain
    handle.assert_mta_running()
    nest = handle.nest
    domain = handle.domain  # fauna.test == the nest's handle domain
    node_url = nest["url"]
    admin_sk = nest["admin"]["signing_key"]
    admin_secret_hex = admin_sk.encode().hex()
    admin_actor_id = bytes(admin_sk.verify_key)

    from conftest import _relaunch_trusting_nest

    _relaunch_trusting_nest(app.driver, nest)

    # ── 1. Log in as the nest admin, landing on conversations. tui has no admin
    # page, so (unlike `login_as_nest_admin`) we navigate straight to
    # conversations; the handle localpart is set so the outbound From is
    # deterministic. `establish` kicks the background silent challenge that
    # refreshes the registry's handle domain (fauna.test) from the nest.
    app.driver.set_state({
        "session": {
            "authenticated": True,
            "node_url": node_url,
            "secret_hex": admin_secret_hex,
            "handle": HANDLE_LOCAL,
            "actor_id": admin_actor_id.hex(),
            "device_id": E2E_LOGIN_DEVICE_ID,
        },
        "nav": {"stack": [{"view": "conversations"}]},
    })

    # ── 2. Enable mail through the real mail-settings UI (the only config
    # surface; testing.md point 8). Mints the admin's MSEK + a credential.
    app.mail_settings.navigate()
    app.mail_settings.ensure_mail_enabled()

    # The client's enable opened the deployment gates, so each idling bridge has
    # exited 0 for the supervisor to restart it bound (mail-bridge-lifecycle.md
    # § Default-off; internal/wsrpc/idle_gate_watch.go). The binaries e2e has no
    # s6, so play supervisor here — until this returns, no listener is bound.
    handle.rebind_after_enable()

    # ── 3. Make `<handle>@<domain>` a routable local mailbox so the stamped From
    # is genuinely replyable (a reply would resolve back to this actor), via the
    # admin's own alias write — the same helper `test_mail_client_receive.py` uses.
    add_exact_alias(node_url, admin_sk, domain, HANDLE_LOCAL)

    # ── 4. Compose + send to an external recipient through the conversations UI.
    token = f"tui-from-{int(time.time() * 1000)}"
    recipient = f"recipient@{EXTERNAL_DOMAIN}"
    app.conversations.start_new_conversation(
        recipient,
        subject=f"Outbound From proof {token}",
        body=f"Body {token} from the tui conversations composer.\n",
    )

    # ── 5. The stub external MX received the relayed message. This gate passes
    # with OR without the fix — the SMTP envelope sender is the authenticated
    # user, so relay never depended on the header From — proving the harness
    # works; only the From assertion below distinguishes the fix.
    deadline = time.monotonic() + 30.0
    relayed = None
    while time.monotonic() < deadline and relayed is None:
        for raw in handle.stub_mx.messages():
            if token.encode() in raw:
                relayed = raw
                break
        if relayed is None:
            time.sleep(0.5)
    assert relayed is not None, (
        f"the stub external MX never received the tui-sent message {token!r} — the "
        f"client-driven send did not relay out. conversations error: "
        f"{app.error_text()!r}; {handle.bridge_log_hint()}"
    )

    # ── 6. The relayed message's `From:` is `<handle>@<handle-domain>`: non-empty
    # (the fix — the SMTP rail's self_address is no longer blank) AND in the local
    # mail domain (routable/replyable, not the loopback default a naive fix would
    # stamp). Before the fix this header is empty.
    from_header = header_value(relayed, "From") or ""
    _, from_addr = email.utils.parseaddr(from_header)
    assert from_addr and "@" in from_addr, (
        f"the tui outbound From must be a real `<handle>@<domain>` address, but was "
        f"empty/malformed ({from_header!r}) — the SMTP rail's self_address gap "
        f"(item 5(b)): the client sent non-replyable mail."
    )
    localpart, _, from_domain = from_addr.partition("@")
    assert from_domain == domain, (
        f"the outbound From domain must be the whoami HANDLE domain {domain!r} "
        f"(== the mail domain here, so replies route back), not {from_domain!r} "
        f"(from {from_addr!r}) — a divergent domain silently loses every reply "
        f"(client-outbound-from-uses-handle-domain)."
    )
    assert localpart, (
        f"the outbound From localpart must be non-empty; got {from_addr!r}"
    )

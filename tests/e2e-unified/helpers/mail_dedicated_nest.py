"""Shared scaffolding for the `dedicated_mail_nest` tier_3 client round-trips.

The `dedicated_mail_nest` fixture stands up a fresh CLAIMED nest whose admin owns
the nest's only mail-enabling actor, and the test injects that admin identity into the
(session-scoped) client via `set_state` rather than driving the full onboarding
UI claim (`mail_client_ui.claim_enable_and_ready` is the UNCLAIMED-nest UI path —
a different fixture family). These helpers are the common preamble every such
test runs: log the client in as the nest admin, resolve the address the client's
WS-RPC layer connects to (the SPA proxy for web), and give the admin a routable
mail address.

Lifted out of `tests/test_mail_enable_then_mua_round_trip.py` (priority #1/#4 —
one admin-inject preamble, not a copy per test) so the Sent-feed round-trip
(`tests/test_mail_sent_feed.py`) reuses the exact same flow.
"""

from __future__ import annotations

import time

from helpers.e2e_session import E2E_LOGIN_DEVICE_ID
from helpers.mail_aliases import add_exact_alias

# Local part aliased to the admin actor so it has a routable mail address. Any
# local part works (the alias maps it to the actor_id); "admin@<domain>" reads
# naturally for the claimed nest's admin.
ADMIN_LOCAL_PART = "admin"


def login_as_nest_admin(app, nest, node_url) -> None:
    """Inject the dedicated nest's admin identity into the (session-scoped) client
    and wait for the authenticated shell — mirrors the `admin_app` fixture, but
    pointed at this test's dedicated nest rather than the shared `nest_instance`.

    `node_url` is the address the client's WS-RPC layer connects to: the raw
    dedicated nest URL for native apps, but the per-test `dedicated_mail_spa_url`
    SPA proxy for web (the browser needs a CORS-bearing origin, and the session
    `spa_url` only proxies `nest_instance` — see that fixture)."""
    from conftest import _relaunch_trusting_nest

    _relaunch_trusting_nest(app.driver, nest)
    admin_secret = nest["admin"]["signing_key"].encode().hex()
    app.driver.set_state(
        {
            "session": {
                "authenticated": True,
                "node_url": node_url,
                "secret_hex": admin_secret,
                # Mirror `admin_app` (which this helper's docstring claims to
                # mirror, and which documents why): the macOS/iOS test agents build
                # the authenticated `FaunaClient` — the `APIClient` every nest-backed
                # VM vends its machine from — only when the patch carries all of
                # node_url + secret_hex + device_id. Omitting it left apple with an
                # authenticated shell and NO client, so `MailSettingsVM.configure`
                # was never reached and every dispatch silently no-opped behind an
                # empty error banner. The agents now default it too (belt and
                # braces), but a real session carries a real device id.
                "device_id": E2E_LOGIN_DEVICE_ID,
            },
            "nav": {"stack": [{"view": "admin"}]},
        }
    )
    if app.driver.is_web():
        # The web SPA's WASM auth + WS-RPC connect to the dedicated nest settle
        # asynchronously; the admin_app fixture sleeps here for the same reason
        # before the test drives any nest-backed UI (enable-mail).
        time.sleep(8)
    app.driver.wait_for("admin-dashboard-heading", timeout=15.0)


def dedicated_node_url(app, handle, request):
    """The address the client connects its WS-RPC layer to for this test's
    dedicated nest: the raw nest URL for native apps, but a per-test SPA proxy for
    web (the browser needs a CORS-bearing origin). Resolved lazily via
    `getfixturevalue` so a native-only run never instantiates the web-only proxy
    (which would build the SPA).

    The proxy is built from **the handle this call was given**, not from a fixed
    fixture name. It used to hardcode `dedicated_mail_spa_url`, which silently
    pointed a web seat at the dedicated *mail* nest however the caller's own nest
    was chosen — so every caller on another dedicated nest (the CalDAV-only nest of
    the mailbox-less rail, the claim-flow variants) had no usable web leg."""
    if app.driver.is_web():
        return request.getfixturevalue("spa_proxy_for")(handle.nest["url"])
    return handle.nest["url"]


def require_caldav_mailbox_mint_supported(driver) -> None:
    """Skip unless this app has the ``enable_caldav_mailbox`` test-agent command
    ``mint_caldav_mailbox`` drives (e2e convention 7 — the platform check lives
    outside the test body).

    linux/macos/tui have it — proven by `test_mail_settings_caldav_only.py`,
    which already mints on all three via this exact command; windows joined
    row 168 (`TestAgent.cs`'s `enable_caldav_mailbox` case), proven by this
    module's own `test_mailbox_less_attendee_materializes_invite_on_events_page`;
    web joined in its catalog trickle-down pass (`$lib/mail-caldav-e2e`).
    ios/android are the remaining cross-app follow-on."""
    if not (
        driver.is_linux()
        or driver.is_macos()
        or driver.is_tui()
        or driver.is_windows()
        or driver.is_web()
    ):
        from helpers.app_surface import skip_unbuilt

        skip_unbuilt(
            driver,
            surface="the enable_caldav_mailbox test-agent command",
            detail="linux/macos/tui have it (test_mail_settings_caldav_only.py "
            "already mints on all three), windows joined row 168 and web "
            "followed; ios/android are the remaining cross-app follow-on",
            tracked="mail-settings.md",
        )


def mint_caldav_mailbox(driver, *, password: str | None = None, timeout: float = 30.0) -> None:
    """Mint the logged-in actor's CalDAV MSEK via the ``enable_caldav_mailbox``
    test-agent command, blocking until the reply lands (the materialize-into-
    calendar key material the ``NestSchedulingSink`` needs, plus — on a
    CalDAV-only/email-disabled nest — the ``default`` credential + canonical
    ``<handle>@<domain>`` AUTH alias a stock CalDAV client logs in with).

    A ``password`` mints a ``default`` credential the test knows (so a stock
    CalDAV client can AUTH as this actor — the organizer's case); ``None``
    generates one (an attendee who never speaks raw CalDAV). The wait sequences
    the mint *before* a CalDAV client AUTHs / an organizer PUTs.

    Lifted out of ``tests/test_caldav_autoschedule_mailbox_less.py`` (priority
    #2/#4 — one CalDAV-mailbox mint path) so the admin-CalDAV-port rebind test
    (``tests/test_caldav_admin_port_rebind.py``) reuses the exact same flow.
    """
    driver.call_command(
        "enable_caldav_mailbox", {"password": password} if password else {}
    )
    deadline = time.monotonic() + timeout
    reply = None
    while time.monotonic() < deadline:
        state = driver.get_state() or {}
        reply = state.get("caldav_mailbox_reply")
        if reply is not None:
            break
        time.sleep(0.3)
    assert reply is not None and reply.get("ok") is True, (
        f"enable_caldav_mailbox must succeed; got {reply!r}"
    )


def alias_admin_to_address(nest, domain: str) -> str:
    """Give the nest admin actor a routable mail address `admin@<domain>` — the
    admin's own alias write, as any member's is — and return that address.

    `EnableMail` provisions the per-actor mail key material; an address beyond
    the canonical `<handle>@<domain>` is the member's to add. Without one the MTA
    `validate_recipient` and the IMAP `resolveActor` can't map the address to the
    admin actor.
    """
    return add_exact_alias(nest["url"], nest["admin"]["signing_key"], domain, ADMIN_LOCAL_PART)

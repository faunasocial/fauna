"""Shared UI-driven mail-bridge scaffolding for the tier_3 client round-trips.

`claim_enable_and_ready` is the common preamble both client-driven mail
round-trip tests run against a fresh UNCLAIMED nest (the `unclaimed_mail_nest_ui`
fixture): claim through the client UI → dismiss the terminal nat_mode_choice
step → assert the handle's domain auto-registered → provision the bits with no
client surface (sealed TLS cert + a spam policy that clears the
DNS-perimeter gates) → wait for the self-enrolled MTA to serve port 25 (the
claim's own derived mail enable auto-approves it — `mail-bridge-lifecycle.md`
§ Onboarding auto-approval) → add the requested credential kind on the
already-enabled mail-settings page → land on the conversations view ready to
compose.

No-modes retirement (ratified 2026-07-12): every nest is sealed at rest
unconditionally now, so there is no storage-mode onboarding question — claim
completion lands the wizard directly on `nat_mode_choice` (the terminal
admin-path step; `docs/goal/behavior/onboarding.md` § 3b-bis), dismissed via
`app.onboarding.finish_nat_mode()`.

Lifted out of `tests/test_mail_client_full_roundtrip.py` (priority #1/#4 — one
claim+enable path, not a copy per test) so `test_mail_client_reply_roundtrip.py`
(the stub-MX auto-reply round-trip) reuses the exact same flow rather than
duplicating ~130 lines of UI dance. The full-roundtrip test keeps its own
send + hand-crafted-reply assertions on top; the reply test layers the stub
auto-reply on top.
"""

from __future__ import annotations

import json
import socket
import time

from clients.ws_rpc_admin_client import WsRpcAdminClient
from helpers.budgets import RPC_ROUNDTRIP_S, UI_SCROLL_SWEEP_S, UI_SETTLE_S
from helpers.mail_wire import _connect_smtp_starttls
from helpers.mail_aliases import add_exact_alias

# The handle local part the UI-claim flow uses; the handle CARRIES its domain
# (`alice@<domain>`) and, once mail is enabled, that handle IS the routable mail
# address (the canonical-handle-alias product behavior).
HANDLE_LOCAL = "alice"

# The password an external MUA would AUTH PLAIN with; only used by the `plain`
# credential variant. The claim's own mailbox mint already holds the `default`
# credential id, so the credential this helper adds takes its own name, and the
# id derives from it: "Mua" → `mua`, "Bearer" → `bearer`.
PLAIN_MUA_PASSWORD = "e2e-mua-secret-pw"
PLAIN_MUA_CREDENTIAL_NAME = "Mua"
OAUTHBEARER_MUA_CREDENTIAL_NAME = "Bearer"


def wait_tcp_accept(port: int, deadline: float) -> bool:
    """Poll until `port` accepts a TCP connection (the bridge bound its port-25
    listener after approval) or the deadline passes."""
    while time.monotonic() < deadline:
        try:
            with socket.create_connection(("127.0.0.1", port), timeout=2.0):
                return True
        except OSError:
            time.sleep(0.5)
    return False


def deliver_inbound(mx_port, server_name, sender_addr, recipient_addr, raw_message, deadline):
    """One real inbound SMTP MAIL/RCPT/DATA over the MTA's port-25 STARTTLS
    listener (an external peer delivering mail). Returns after the 250 on `.`,
    which the MTA sends only once the WS-RPC ingest (seal + store) committed."""
    with _connect_smtp_starttls(mx_port, server_name, deadline) as conn:
        conn.cmd(f"MAIL FROM:<{sender_addr}>", "250", deadline)
        conn.cmd(f"RCPT TO:<{recipient_addr}>", "250", deadline)
        conn.cmd("DATA", "354", deadline)
        conn.send_raw(raw_message)
        conn.cmd(".", "250", deadline)
        conn.cmd("QUIT", "221", deadline)


def route_inbound_mail_to_app(app, mail_bridge_mta, nest_instance, test_user, local_part):
    """Make ``<local_part>@<bridge domain>`` deliver to the logged-in app's user, and
    return that address — the preamble of every test that delivers a real inbound
    mail to the app on the session's ``mail_bridge_mta`` (lifted out of
    ``test_mail_client_receive.py``, 2026-09-21).

    Two steps, because enabling mail and routing an address are separate facts:
    enabling (through the app's own mail-settings page) mints the client-held
    MSEK and registers its recipient pubkey on the nest, so the MTA can seal
    inbound mail to a key only this app can open; the alias (the member's own
    write, ``helpers.mail_aliases.add_exact_alias``) is what
    ``validate_recipient`` resolves on RCPT TO. Give each test its own
    ``local_part`` so no two tests contend for one alias."""
    app.mail_settings.navigate()
    app.mail_settings.ensure_mail_enabled()
    return add_exact_alias(
        nest_instance["url"], test_user["signing_key"], mail_bridge_mta.domain, local_part
    )


def plain_message(sender_addr: str, recipient_addr: str, nonce: str) -> bytes:
    """A plain-text RFC 5322 message carrying ``nonce`` in its subject and body,
    so the conversations list can be searched for it."""
    return (
        "\r\n".join(
            [
                f"From: External Sender <{sender_addr}>",
                f"To: {recipient_addr}",
                f"Subject: Inbound {nonce}",
                f"Message-ID: <{nonce}@{sender_addr.split('@', 1)[1]}>",
                "Date: Mon, 21 Sep 2026 12:00:00 +0000",
                "MIME-Version: 1.0",
                "Content-Type: text/plain; charset=utf-8",
                "",
                f"The {nonce} body.",
            ]
        )
        + "\r\n"
    ).encode()


def thread_with_nonce(app, nonce: str):
    """The conversations-list thread whose label or snippet carries ``nonce``."""
    return next(
        (
            t
            for t in app.conversations.list_threads()
            if nonce in (t.label or "") or nonce in (t.snippet or "")
        ),
        None,
    )


def wait_for_thread_with_nonce(app, nonce: str, *, what: str, budget_s: float, bridge_log=None):
    """Wait until the mail tagged ``nonce`` shows decrypted in Conversations."""
    from helpers.waiting import wait_until

    return wait_until(
        lambda: thread_with_nonce(app, nonce),
        budget_s,
        diagnose=lambda: (
            f"{what}: the email tagged {nonce!r} never showed decrypted in the "
            f"conversations list.\n"
            f"  threads: {[(t.label, t.snippet, t.rail) for t in app.conversations.list_threads()]}\n"
            f"  conversations error: {app.error_text()!r}\n"
            f"  bridge log: {bridge_log}"
        ),
    )


def _add_credential_ui(app, credential_kind):
    """Add one credential of `credential_kind` to the mailbox the claim already
    minted, through the mail-settings add-credential form, and wait for its row.

    Mail is ON before this runs: a real-domain handle derives
    `email_enable_requested()`, so the post-claim launch glue minted the admin's
    mailbox — recipient key, MSEK and a generated-password `default` credential —
    and fired `set_mail_enabled(true)` (`mail-bridge-lifecycle.md` § Default-off on
    first claim, step 1). So this ASSERTS mail is on rather than enabling it. The
    enable toggle on an already-enabled mailbox opens the disable confirm, and an
    app that skipped the claim's enable must fail here, not be rescued by a second
    enable. The kind axis lives on the added credential instead — the same "I added
    a MUA password in the client" gesture `caldav_onboarding` makes on this path.
    """
    app.mail_settings.navigate()
    assert app.mail_settings.wait_for_enabled_toggle_state("on", timeout=UI_SETTLE_S), (
        f"the claim should have enabled mail (a real-domain handle derives "
        f"email_enable_requested, onboarding.md § 3b), but the mail-settings toggle "
        f"reads {app.mail_settings.enabled_toggle_state()!r}; "
        f"status={app.mail_settings.status_text()!r}; "
        f"error: {app.mail_settings.page_error_text(timeout=2.0)!r}"
    )
    assert app.mail_settings.wait_for_credential_count_at_least(1, timeout=UI_SETTLE_S), (
        "the claim's mailbox mint should leave its generated `default` credential; "
        f"error: {app.mail_settings.page_error_text(timeout=2.0)!r}"
    )
    before = app.mail_settings.credential_count()
    if credential_kind == "plain":
        app.mail_settings.add_credential_plain(PLAIN_MUA_CREDENTIAL_NAME, PLAIN_MUA_PASSWORD)
    elif credential_kind == "oauthbearer":
        token = app.mail_settings.add_credential_oauthbearer(OAUTHBEARER_MUA_CREDENTIAL_NAME)
        assert token, (
            "adding an OAUTHBEARER credential should reveal its one-time token; "
            f"error: {app.mail_settings.page_error_text(timeout=2.0)!r}"
        )
        app.mail_settings.close_add_credential_form()
    else:
        raise ValueError(f"unknown credential_kind {credential_kind!r}")
    assert app.mail_settings.wait_for_credential_count_at_least(before + 1, timeout=UI_SETTLE_S), (
        f"adding a {credential_kind} credential should grow the list past {before}; "
        f"error: {app.mail_settings.page_error_text(timeout=2.0)!r}"
    )


def _mta_enrollment_state(nest, admin_sk) -> str:
    """Say why the box's MTA is not serving, for a failure message: whether the
    deployment's mail is on (the auto-approval condition) and whether the MTA
    still sits in the pending list. Read over Admin WS-RPC, never asserted on."""
    try:
        admin_ws = WsRpcAdminClient(
            nest.nest_url,
            actor_id=bytes(admin_sk.verify_key),
            signing_key=bytes(admin_sk),
        )
        with admin_ws:
            cfg = admin_ws.call("fauna.bridges.get_mail_config", {})
            pending = admin_ws.call("fauna.bridges.list_pending_bridges", {})
    except Exception as e:  # noqa: BLE001 — a diagnosis must never mask the failure.
        return f"enrollment state unreadable: {e!r}"
    still_pending = any(
        bytes(b.get("ed25519_pubkey") or b"").hex() == nest.bridge_ed_pubkey_hex
        for b in pending.get("bridges", [])
    )
    return f"mail_enabled={cfg.get('mail_enabled')!r}, MTA still pending={still_pending}"


def claim_to_nat_mode_page(app, nest, *, handle, admin_secret_hex, node_url):
    """Drive the UI claim of a fresh unclaimed nest up to (but NOT past)
    `nat_mode_choice`, landing on the terminal admin-path page WITHOUT
    dismissing it. The shared front-half of every UI claim flow.

    Steps: navigate to identity choice → import `admin_secret_hex` via the
    paste-key flow → fake ONLY the DNS discovery by jumping the wizard to the
    claim-code page with this LOCAL nest's `node_url` + `handle` (everything
    downstream is the REAL client flow — the claim-code submit does a real POST
    /api/v1/claim-admin, and the handle's domain is sent as the claim's
    `mail_domain`, so the nest auto-registers it) → submit the claim code.

    `node_url` is the address the APP dials, which is not always the nest's own:
    the raw nest URL for a native app, but the nest's SPA proxy for web — a
    browser cannot reach a raw nest, which sends no CORS headers
    (`mail_dedicated_nest.dedicated_node_url` resolves the right one). It is a
    required argument rather than a `nest.nest_url` default so a new web caller
    cannot inherit the unreachable address by omission.

    No-modes retirement (ratified 2026-07-12): the admin claim now lands the
    wizard directly on `nat_mode_choice` — every nest is sealed at rest
    unconditionally, so there is no intervening storage-mode question. The
    caller dismisses it via `app.onboarding.finish_nat_mode()`.

    Reused by both `claim_enable_and_ready` and the caldav-onboarding helper —
    priority #2/#4, one claim path, not a copy.
    """
    from conftest import _relaunch_trusting_nest

    _relaunch_trusting_nest(app.driver, nest.nest)
    ob = app.onboarding
    ob.navigate_to_status()
    ob.import_key(admin_secret_hex)
    app.driver.wait_for("handle-input", timeout=15)
    app.driver.call_machine_method(
        "navigate_to_claim_code_for_known_nest",
        json.dumps([node_url, handle]),
    )
    app.driver.wait_for("claim-code-input", timeout=20)
    app.driver.clear_and_type("claim-code-input", nest.claim_code)
    app.driver.click("claim-code-submit-button")
    # The admin claim lands the wizard on nat_mode_choice; wait for its
    # canary so the caller can dismiss it via finish_nat_mode().
    app.driver.wait_for("nat-mode-confirm-button", timeout=20)


def claim_enable_and_ready(app, nest, *, credential_kind):
    """Drive the shared UI preamble of a client-driven mail round-trip against a
    fresh unclaimed nest, parametrized over credential kind, and leave the
    client logged in as admin on the conversations view, ready to compose.
    Returns ``{admin_secret_hex, handle, domain}``.

    Steps (the staged flow shared by both client round-trip tests):
      1. claim the unclaimed nest through the client UI (becoming admin) and
         dismiss the terminal nat_mode_choice step; assert the handle's domain
         auto-registered;
      2. provision the bits with no client surface (TLS cert + spam policy);
      3. wait for the self-enrolled MTA to serve port 25 — the claim's derived
         mail enable auto-approved it, so there is no card to click;
      4. add a credential of `credential_kind` to the claim's already-enabled
         mailbox; land on the conversations view.
    """
    from nacl.signing import SigningKey

    domain = nest.domain                          # fauna.test
    handle = f"{HANDLE_LOCAL}@{domain}"           # the handle CARRIES its domain

    # The admin identity: generated by the test (so we hold the key for the
    # scaffolding WS-RPC) and imported through the client's onboarding paste-key
    # flow — still a real UI claim.
    admin_sk = SigningKey.generate()
    admin_secret_hex = bytes(admin_sk).hex()

    # ── 1. Claim the unclaimed nest through the client UI ───────────────────
    # (Front-half shared with the caldav-onboarding helper: navigate → import →
    # fake-DNS jump to claim-code → submit → land on nat_mode_choice. The
    # handle carries `@fauna.test`, so the claim auto-registers the domain —
    # asserted below.)
    # The raw nest URL: every caller gates web out (`skip_unbuilt`), and a web
    # leg would thread `dedicated_node_url` here and into the `set_state` below,
    # as `caldav_onboarding.onboard_and_enable` does.
    claim_to_nat_mode_page(
        app, nest, handle=handle, admin_secret_hex=admin_secret_hex,
        node_url=nest.nest_url,
    )

    # Dismiss the terminal nat_mode_choice step (confirm-only common case —
    # accepts the pre-selected seed) — this is what exits the wizard to LoggedIn.
    app.onboarding.finish_nat_mode()
    time.sleep(2.0)  # let the authenticated shell + WS session to this nest settle

    # Nudge the linux app to (re)build the authenticated window WITH the admin
    # shell by re-asserting the same session — the established admin-e2e mechanism
    # (the admin shell's root stack child is built on a `session` patch). This does
    # NOT re-claim; the claim already happened through the UI.
    app.driver.set_state({
        "session": {
            "authenticated": True,
            "node_url": nest.nest_url,
            "secret_hex": admin_secret_hex,
        },
        "nav": {"stack": [{"view": "admin"}]},
    })
    app.driver.wait_for("admin-dashboard-heading", timeout=20.0)

    # (Still part of step 1.) The handle's domain was auto-registered at claim
    # — ASSERT it ──────────────────────────────────────────────────────────
    app.admin.navigate_dns()
    # UI_SCROLL_SWEEP_S, not RPC_ROUNDTRIP_S: on windows this row sits below a
    # full credentials section + add-domain form in ONE ScrollViewer with no
    # ScrollItemPattern support, so `wait_for` falls back to a percent-sweep
    # (measures well under a second per sweep here — the budget is generous,
    # not a measured floor; the repeat-poll failures were a genuine
    # layout bug, fixed in `AdminDnsPage.xaml`).
    app.driver.wait_for("admin-dns-domain-name", timeout=UI_SCROLL_SWEEP_S)
    deadline = time.monotonic() + 10.0
    seen_domain = False
    while time.monotonic() < deadline and not seen_domain:
        n = app.driver.count("admin-dns-domain-name")
        for i in range(n):
            if domain in (app.driver.get_text("admin-dns-domain-name", index=i) or ""):
                seen_domain = True
                break
        if not seen_domain:
            time.sleep(0.5)
    assert seen_domain, (
        f"the claim-handle domain {domain!r} was not auto-registered on the "
        f"admin-dns page (claim_admin_core::ensure_mail_domain_registered). "
        f"error: {app.error_text()!r}"
    )

    # ── 2. Scaffolding the UI can't express (no real cert / DNS for a test
    # domain): provision the bridge's sealed TLS cert + clear the
    # DNS-perimeter gates, over Admin WS-RPC with the key we control. (The
    # nest minted the domain's DKIM key when the claim registered the domain.)
    admin_ws = WsRpcAdminClient(
        nest.nest_url,
        actor_id=bytes(admin_sk.verify_key),
        signing_key=bytes(admin_sk),
    )
    with admin_ws:
        admin_ws.provision_tls_cert_blob(nest.tls_blob)
        admin_ws.call(
            "fauna.bridges.put_spam_policy",
            {
                "baseline_standing_publish": False,
                "dnsbl_servers": [],
                "greylist_enabled": False,
                "greylist_delay_secs": 0,
                "fcrdns_mode": "off",
                "max_conn_per_min": 1000,
            },
        )

    # ── 3. The claim's own enable approved the box's MTA — no card to click ────
    # The enable in step 1 IS the approval: the nest's `request_enrollment` lands a
    # loopback MTA `approved` once `mail_enabled` is true, and a row that enrolled
    # `pending` before the enable self-heals on its next poll
    # (`mail-bridge-lifecycle.md` § Onboarding auto-approval). This step used to
    # wait for an admin-bridges-pending card to click, which the product correctly
    # never shows on this path. The observable is the MTA serving.
    assert wait_tcp_accept(nest.mx_port, time.monotonic() + 45.0), (
        f"the box's own MTA never bound its port-25 listener on {nest.mx_port} "
        f"after the claim's mail enable ({_mta_enrollment_state(nest, admin_sk)}; "
        f"{nest.bridge_log_hint()})"
    )

    # ── 4. Add the requested credential kind to the claim's mailbox ─────────
    _add_credential_ui(app, credential_kind)

    # Re-assert the session to land on conversations for the send; the account
    # cache (handle + mail domain) was populated by the claim's silent challenge,
    # so ensure_smtp_backend registers the SMTP rail with `<handle>@<domain>`.
    app.driver.set_state({
        "session": {
            "authenticated": True,
            "node_url": nest.nest_url,
            "secret_hex": admin_secret_hex,
        },
        "nav": {"stack": [{"view": "conversations"}]},
    })
    time.sleep(3.0)

    # Every caller goes on to assert a REAL message surfaces, which windows can only
    # do with its real receive loop — started solely under the session-wide
    # `real_conversations` marker (conftest `_apply_real_conversations_env`).
    # Unmarked, windows keeps its mock backend and the caller's wait fails SILENTLY:
    # an empty thread list, no error, the MTA log showing the message ingested. windows is the
    # one flag-gated app that publishes a readiness signal, so fail here, naming
    # the cause. (linux/web/tui run the real session for every login; macOS/iOS/
    # android publish no signal — `enable_real_faunamls`'s no-op branch.)
    if app.driver.is_windows():
        try:
            app.conversations.enable_real_faunamls(timeout_s=RPC_ROUNDTRIP_S)
        except AssertionError as e:
            raise AssertionError(
                "windows is running its MOCK conversations backend, so no real "
                "delivery can surface: the calling module needs "
                "`pytest.mark.real_conversations` (conftest "
                "`_apply_real_conversations_env` sets FAUNA_E2E_REAL_CONVERSATIONS "
                f"only for an invocation that collects a marked test). {e}"
            ) from e

    return {"admin_secret_hex": admin_secret_hex, "handle": handle, "domain": domain}

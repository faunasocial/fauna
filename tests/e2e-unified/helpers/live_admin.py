"""Shared UI-only admin steps for the `live_box` tests (tier_4, example.com).

Every live-box test reaches the admin shell the same way — import the admin
secret, type a handle, run the REAL handle check, then branch on what the
wizard surfaces (sign in on a box we already administer, claim on an
unclaimed one) — and the mail-bearing ones then enable mail and approve the
co-located bridges through the same admin UI. These steps were copied verbatim
between ``test_mail_zero_cheat_live.py`` and ``test_activitypub_live.py``;
they live here once so the bootstrap test (``test_live_box_bootstrap.py``) and
both siblings drive one shape.

Nothing here calls a nest API or a test-hooks route: every mutation is an app
UI action (convention 8), every page is reached by clicks — the tab, then the
shell rail's ``admin-nav-row[…]`` / ``settings-nav-row[…]`` row, never the nav
patch — and every wait polls observable state (convention 14). The rail-row ids
are on tui today; the other six apps gain them in trickle-down rows, so a live
run on one of those fails at the first rail click until then. Nothing here factory-resets — a caller that wants a reset does it itself,
visibly (the shared-box carve-out, ``testing.md`` § The shared-box rule).
"""

from __future__ import annotations

import os
import smtplib
import socket
import ssl
import time

import pytest

from helpers.live_handle import sign_in_handle


def dump(app, ids) -> str:
    """Visibility of ``ids`` plus the accessibility tree — the self-diagnosing
    tail every live-box failure message carries."""
    vis = {}
    for eid in ids:
        try:
            vis[eid] = app.driver.is_visible(eid)
        except Exception as e:
            vis[eid] = f"<err {e}>"
    try:
        tree = app.driver.tree()
    except Exception as e:
        tree = f"(tree dump failed: {e})"
    return f"visibility: {vis}\n--- accessibility tree ---\n{tree}"


def wait_connected(app, timeout: float = 90.0) -> None:
    """Wait until the sidebar connection-status indicator reads Connected —
    the authed shell renders before ``start_ws_rpc()``'s async ``connect()``
    completes, so driving any WS-RPC-backed UI before this would hit
    "rpc disconnected"."""
    deadline = time.monotonic() + timeout
    last = ""
    while time.monotonic() < deadline:
        try:
            last = app.driver.get_text("connection-status") or ""
        except Exception:
            last = ""
        if last.startswith("Connected"):
            return
        time.sleep(1.5)
    pytest.fail(
        f"WS-RPC never reached Connected within {timeout:.0f}s (last status: "
        f"{last!r}) — the post-onboarding session did not establish a live "
        f"connection.\n" + dump(app, ("connection-status", "error-message", "feed-tab"))
    )


def wait_logged_in(app, timeout: float = 150.0) -> None:
    """Wait for the feed view, driving the launch screen's retry on a
    transient connect error (a cold launch against a remote nest can surface
    one)."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if app.driver.is_visible("feed-view") or app.driver.is_visible("feed-tab"):
            return
        if app.driver.is_visible("launch-retry-button"):
            try:
                app.driver.click("launch-retry-button")
            except Exception:
                pass
        time.sleep(2.0)
    pytest.fail(
        f"client never reached the feed view within {timeout:.0f}s.\n"
        + dump(app, ("feed-view", "feed-tab", "launch-retry-button",
                     "launch-transient-error", "error-message",
                     "nat-mode-confirm-button", "claim-code-input"))
    )


def reach_admin_shell(
    app,
    *,
    nest_url: str,
    secret_hex: str,
    claim_code: str = "",
    nat_mode: str | None = None,
) -> str:
    """Import the admin identity and reach the logged-in shell purely through
    the wizard, branching on whatever the real handle check surfaces. Returns
    ``"claimed-fresh"`` or ``"signed-in"``.

    The typed handle is ``sign_in_handle(nest_url)``: the caller's override
    (``FAUNA_LIVE_HANDLE`` / ``FAUNA_LIVE_MAIL_ADDRESS``) when set, else a
    probe localpart on the box's own host. On an already-registered identity
    the nest's silent challenge answers with the REGISTERED handle and the
    wizard signs in as that one, so what gets typed is a routing input, not an
    identity claim (``helpers/live_handle.py``).

    ``nat_mode`` (``"public"``/``"private"``) is the NAT-mode choice made on a
    claim; ``None`` accepts the page's pre-selected seed.
    """
    ob = app.onboarding
    typed = sign_in_handle(nest_url)
    ob.navigate_to_status()
    ob.import_key(secret_hex)        # → handle_entry
    ob.fill_handle(typed)
    ob.run_handle_check(timeout=60)  # real DoH probe of the handle's domain
    ob.submit_handle()

    deadline = time.monotonic() + 120.0
    while time.monotonic() < deadline:
        if app.driver.is_visible("feed-view") or app.driver.is_visible("feed-tab"):
            return "signed-in"  # already claimed by us → wizard signed in
        if app.driver.is_visible("claim-code-input"):
            # A probe handle must never CLAIM a box: the claim would bake the
            # probe in as the admin's real handle — and, through its domain
            # suffix, as the deployment's identity (claim_core.rs) — which is
            # damage no teardown can undo. Claiming is an explicit-handle act.
            if not os.environ.get("FAUNA_LIVE_HANDLE") and not os.environ.get(
                "FAUNA_LIVE_MAIL_ADDRESS"
            ):
                pytest.fail(
                    "the nest is UNCLAIMED (claim-code page reached) and the "
                    f"handle typed was the derivation probe {typed!r}. Claiming "
                    "would register that probe as the admin's real handle — "
                    "refusing. A claim-fresh run must name its handle: set "
                    "FAUNA_LIVE_HANDLE (plus FAUNA_LIVE_CLAIM_CODE)."
                )
            if not claim_code:
                pytest.fail(
                    "the nest is UNCLAIMED (claim-code page reached) but "
                    "FAUNA_LIVE_CLAIM_CODE is not set."
                )
            app.driver.clear_and_type("claim-code-input", claim_code)
            app.driver.click("claim-code-submit-button")
            # NAT-mode choice — the terminal admin-path step, reached
            # directly on claim completion (no-modes retirement, 2026-07-12).
            app.driver.wait_for("nat-mode-confirm-button", timeout=60)
            app.onboarding.finish_nat_mode(mode=nat_mode)
            wait_logged_in(app)
            return "claimed-fresh"
        if app.driver.is_visible("launch-retry-button"):
            try:
                app.driver.click("launch-retry-button")
            except Exception:
                pass
        time.sleep(2.0)

    pytest.fail(
        "after the real handle check, the wizard reached neither the feed "
        "(signed-in) nor the claim-code page (unclaimed) within 120s.\n"
        + dump(app, ("feed-view", "feed-tab", "claim-code-input",
                     "handle-message-area", "error-message",
                     "launch-retry-button"))
    )


def enable_mail(app, password: str) -> bool:
    """Enable mail (PLAIN password) through the mail-settings UI, reached by
    clicks. Idempotent: returns False without mutating when a credential already
    exists, True when this call enabled it."""
    app.mail_settings.open_by_click()
    if app.mail_settings.wait_for_credential_count_at_least(1, timeout=12.0):
        return False  # already enabled
    app.mail_settings.enable_mail_plain(password, display_name="Default")
    assert app.mail_settings.wait_for_credential_count_at_least(1, timeout=40.0), (
        "enabling mail did not mint a credential. mail-page error: "
        f"{app.mail_settings.page_error_text(timeout=3.0)!r}; "
        f"app error: {app.error_text()!r}"
    )
    return True


def approve_pending_bridges(app, *, timeout: float = 150.0) -> int:
    """Approve the co-located mail bridge(s) through the admin UI; returns how
    many approve clicks landed.

    The MDA/MTA cold-boot and ``request_enrollment`` **asynchronously** — the
    pending row can appear tens of seconds after mail is enabled. So poll
    (re-opening the page by its rail row each round to refresh the list) until a pending row
    appears, approve every pending row, and return only once the list has
    drained *after* at least one approval — a late-enrolling bridge is still
    caught. Returns 0 at the deadline if nothing ever enrolled; the caller's
    own serving wait is the real assertion."""
    deadline = time.monotonic() + timeout
    approved = 0
    while time.monotonic() < deadline:
        app.admin.open_bridges_pending_by_click()
        n = app.driver.count("admin-bridges-pending-pubkey-hex")
        if n == 0:
            if approved:
                return approved  # approved + the pending list has drained
            time.sleep(3.0)
            continue  # bridge hasn't enrolled yet — keep waiting
        for i in range(n):
            try:
                app.driver.click("admin-bridges-pending-approve-button", index=i)
                approved += 1
            except Exception:
                pass
        time.sleep(3.0)
    return approved


def wait_port_open(host: str, port: int, timeout: float = 150.0) -> None:
    """Poll until ``host:port`` accepts a TCP connection."""
    import socket

    deadline = time.monotonic() + timeout
    last = ""
    while time.monotonic() < deadline:
        try:
            with socket.create_connection((host, port), timeout=4.0):
                return
        except OSError as e:
            last = str(e)
            time.sleep(3.0)
    pytest.fail(
        f"{host}:{port} never accepted a connection within {timeout:.0f}s "
        f"(last: {last})."
    )


# ── strict-TLS mail-port probes (stdlib, default trust store) ──────────────
# Lifted from test_live_box_bootstrap.py so the staging-box provisioning
# test drives the same probes.


def strict_tls_greets(host: str, port: int) -> tuple[bool, str]:
    """Implicit-TLS port (993/465): strict handshake for SNI ``host``, then the
    protocol greeting (``* OK`` / ``220``)."""
    try:
        with socket.create_connection((host, port), timeout=10) as raw:
            with ssl.create_default_context().wrap_socket(raw, server_hostname=host) as tls:
                tls.settimeout(15)
                banner = tls.recv(512).decode("utf-8", "replace").strip()
    except (OSError, ssl.SSLError) as e:
        return False, f"{type(e).__name__}: {e}"
    ok = banner.startswith("* OK") if port == 993 else banner.startswith("220")
    return ok, banner[:120]


def strict_starttls_587(host: str) -> tuple[bool, str]:
    try:
        with smtplib.SMTP(host, 587, timeout=15) as smtp:
            smtp.ehlo()
            code, msg = smtp.starttls(context=ssl.create_default_context())
            smtp.ehlo()
            return code == 220, f"STARTTLS {code} {msg!r}"
    except (OSError, ssl.SSLError, smtplib.SMTPException) as e:
        return False, f"{type(e).__name__}: {e}"


def strict_mail_ports(host: str) -> dict[int, tuple[bool, str]]:
    return {
        993: strict_tls_greets(host, 993),
        465: strict_tls_greets(host, 465),
        587: strict_starttls_587(host),
    }

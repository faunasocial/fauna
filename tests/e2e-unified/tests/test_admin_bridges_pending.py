"""tier_3 e2e: the admin bridges-pending page renders pending mail-bridge
approval cards and the admin can approve one (``admin-bridges-pending-card`` /
``-pubkey-hex`` / ``-requested-role`` / ``-approve-button``).

This is the client-side counterpart to ``tests/api/test_mail_bridge_approval.py``
(which proves the WS-RPC wire). Here a pending bridge is seeded over the bridge's
own anonymous enrollment kind (``fauna.bridges.request_enrollment``), then
the client's admin bridges-pending page must surface it through the shared ``BridgeApprovalMachine``
(``libs/fauna-client-mail-settings::bridge_approval``) over WS-RPC, and approving
it must drop it from the pending feed.

Each test runs on its own dedicated nest (``bridges_pending_nest``), not the
session ``nest_instance``: the nest auto-approves a loopback mail bridge's
enrollment once an admin has enabled mail or a DAV axis
(``mail-bridge-lifecycle.md`` § Onboarding auto-approval), and the session nest
has those enabled by the mail fixtures — so a pending MTA/MDA card exists only on
a nest whose axes are still off, which a fresh nest is. Toggling the axes off on
the session nest instead would restart its running MDA (a config change bounces
the bridge). We still seed a random pubkey and assert *that* card rather than an
absolute count.
"""

import secrets
import time

import pytest

from i18n.strings import S
from helpers.bridge_enrollment import enroll_bridge

# Scoped to the apps that render the per-role `admin-bridges-pending-card-name`
# (admin.md § Bridge display naming): linux (lead) + macOS/iOS (the
# shared FaunaKit lift) + windows (the `admin-bridges-pending-card-name` on the
# WinUI approval card, mapped via the shared UniFFI bridge_display_name) + android (the
# bridgeDisplayName mapping in AdminBridgesPendingScreen.kt; its `--client android`
# run is host-emulator-gated) + web (the `bridgeDisplayName` mapping atop each
# `admin-bridges-pending-card` in admin/bridges-pending/+page.svelte) — the same
# lead+lifted pattern test_admin_calendar uses. The approve flow's wire is also
# covered client-independently by tests/api/test_mail_bridge_approval.py.
# `tui` added 2026-07-29 with the page itself (`apps/fauna-tui/src/admin/bridges.rs`) —
# the last app owing `admin-bridges-pending` per `admin.md` § Bridge display naming.
pytestmark = [
    pytest.mark.tier_3,
    pytest.mark.linux,
    pytest.mark.macos,
    pytest.mark.ios,
    pytest.mark.windows,
    pytest.mark.android,
    pytest.mark.web,
    pytest.mark.tui,
]


@pytest.fixture
def bridges_pending_nest(request, nest_mode, tmp_path_factory):
    """A dedicated, function-scoped, claimed nest with every enable axis still off,
    so a mail bridge's enrollment lands ``pending`` (module docstring)."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(request, nest_mode, tmp_path_factory, "bridges-pending")
    try:
        yield nest
    finally:
        cleanup()


@pytest.fixture
def bridges_pending_spa_url(static_dir, bridges_pending_nest):
    """Function-scoped SPA proxy → ``bridges_pending_nest`` (the session
    ``spa_url`` only proxies ``nest_instance``). Mirrors
    ``registration_posture_spa_url``."""
    from conftest import _serve_spa_proxy

    url, server = _serve_spa_proxy(static_dir, bridges_pending_nest["url"])
    yield url
    server.shutdown()


@pytest.fixture
def bridges_pending_admin_app(request, app, bridges_pending_nest):
    """``admin_app``, pointed at ``bridges_pending_nest`` (`_login_admin_as` — only
    the nest differs). Mirrors ``registration_posture_admin_app``."""
    from conftest import _login_admin_as

    _login_admin_as(
        app,
        request,
        bridges_pending_nest,
        spa_url_fixture="bridges_pending_spa_url",
        fixture_name="bridges_pending_admin_app",
    )
    yield app


def _seed_pending_bridge(nest_instance, role: str = "mta") -> tuple[str, str]:
    """Enroll a pending bridge over the bridge's own anonymous
    ``request_enrollment`` kind. Nothing proves possession of the key at
    enrollment, so a synthetic 32-byte value is enough to exercise the approval
    card. Returns ``(ed25519_pubkey_hex, bridge_id)``. Mirrors
    ``tests/api/test_mail_bridge_approval.py::_seed_pending``."""
    pubkey = secrets.token_bytes(32)
    bridge_id = f"{role}-{secrets.token_hex(4)}"
    status = enroll_bridge(nest_instance["url"], pubkey, role, bridge_id)
    assert status == "pending", (
        f"seed must land pending on a nest with its enable axes off, got {status!r}"
    )
    return pubkey.hex(), bridge_id


def _find_card(admin_app, pubkey_hex: str) -> tuple[int | None, int]:
    """Index of the card whose pubkey-hex contains ``pubkey_hex`` (+ total count)."""
    count = admin_app.driver.count("admin-bridges-pending-pubkey-hex")
    for i in range(count):
        txt = admin_app.driver.get_text("admin-bridges-pending-pubkey-hex", index=i) or ""
        if pubkey_hex in txt:
            return i, count
    return None, count


@pytest.mark.feature("admin-bridges")
def test_admin_bridges_pending_lists_and_approves(bridges_pending_admin_app, bridges_pending_nest):
    """A bridge enrolled as pending shows up as an approval card; clicking
    approve drops it from the pending feed."""
    admin_app = bridges_pending_admin_app
    pubkey_hex, _ = _seed_pending_bridge(bridges_pending_nest, "mta")

    admin_app.admin.navigate_bridges_pending()

    # Poll for the seeded card to render (the fetch is an async WS-RPC round-trip).
    deadline = time.time() + 10.0
    idx = None
    while time.time() < deadline:
        idx, _ = _find_card(admin_app, pubkey_hex)
        if idx is not None:
            break
        time.sleep(0.5)
    assert idx is not None, (
        f"seeded pending bridge {pubkey_hex[:16]}… not found among "
        f"{admin_app.driver.count('admin-bridges-pending-pubkey-hex')} card(s) "
        f"(error: {admin_app.driver.get_text('error-message') if admin_app.has_error() else 'none'})"
    )

    # The card surfaces the requested role.
    role_txt = admin_app.driver.get_text("admin-bridges-pending-requested-role", index=idx) or ""
    assert "mta" in role_txt.lower(), f"role text {role_txt!r} should mention mta"

    # The card shows the friendly per-role display name (admin.md § Bridge display
    # naming): an MTA serves SMTP only, so its name is "Mail bridge" (no calendar).
    name_txt = admin_app.driver.get_text("admin-bridges-pending-card-name", index=idx) or ""
    assert name_txt.strip() == S.admin.bridges_pending.name_mail, (
        f"mta card name {name_txt!r} should be {S.admin.bridges_pending.name_mail!r}"
    )

    # Approve it → it leaves the pending feed (the machine re-reads after the
    # mutation, so the rebuilt card list no longer contains this pubkey).
    admin_app.driver.click("admin-bridges-pending-approve-button", index=idx)

    deadline = time.time() + 10.0
    gone = False
    while time.time() < deadline:
        again, _ = _find_card(admin_app, pubkey_hex)
        if again is None:
            gone = True
            break
        time.sleep(0.5)
    assert gone, (
        f"approved bridge {pubkey_hex[:16]}… still in the pending feed "
        f"(error: {admin_app.driver.get_text('error-message') if admin_app.has_error() else 'none'})"
    )


@pytest.mark.feature("admin-bridges")
def test_mda_card_names_mail_and_calendar(bridges_pending_admin_app, bridges_pending_nest):
    """An MDA bridge serves IMAP **and** CalDAV, so its approval card names calendar:
    the friendly per-role display name is "Mail & calendar bridge" (admin.md
    § Bridge display naming; bridges.md § Active bridges). This is the calendar
    half of the per-role naming the mta test covers."""
    admin_app = bridges_pending_admin_app
    pubkey_hex, _ = _seed_pending_bridge(bridges_pending_nest, "mda")

    admin_app.admin.navigate_bridges_pending()

    deadline = time.time() + 10.0
    idx = None
    while time.time() < deadline:
        idx, _ = _find_card(admin_app, pubkey_hex)
        if idx is not None:
            break
        time.sleep(0.5)
    assert idx is not None, (
        f"seeded pending mda bridge {pubkey_hex[:16]}… not found "
        f"(error: {admin_app.driver.get_text('error-message') if admin_app.has_error() else 'none'})"
    )

    name_txt = admin_app.driver.get_text("admin-bridges-pending-card-name", index=idx) or ""
    assert name_txt.strip() == S.admin.bridges_pending.name_mail_calendar, (
        f"mda card name {name_txt!r} should be {S.admin.bridges_pending.name_mail_calendar!r}"
    )

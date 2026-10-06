"""admin-files page (admin.md § Files — the deployment-wide WebDAV-enable
toggle, the files sibling of admin-contacts's CardDAV-enable toggle;
webdav-server.md § Independent enablement, client slice 6a).

The shared `fauna_client_mail_settings::webdav_policy::WebdavPolicyMachine` is
the one orchestrator; each app lifts it over the `build_webdav_policy_machine`
UniFFI/wasm export and renders its snapshot (same consumer model as
admin-contacts — see test_admin_contacts.py's per-app lift notes).

tier_3: a real client driver against a real `fauna-nest` binary. The round-trip
test drives the toggle (flip via the UI → shared machine `set_webdav_enabled` →
nest → re-hydrate via `get_mail_config` → re-render) and asserts ground truth
over the same Admin WS-RPC `fauna.bridges.get_mail_config` twin the page hydrates
from (its `webdav_enabled` field) — not UI introspection alone. No nest work
backs this page: `set_webdav_enabled` + the MDA gate landed in; this
is the admin-UI surface. No port sibling — WebDAV rides the shared DAV listener
the admin-calendar port field governs. WebDAV has no per-actor mailbox (unlike
CardDAV), so there is no mailbox-provision leg.
"""
import time

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient

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


def _admin_client(nest_instance):
    """An Admin WS-RPC client keyed on the nest's admin identity — the ground-truth
    read path for the round-trip (mirrors test_admin_contacts)."""
    admin = nest_instance["admin"]
    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )


def _wait(predicate, timeout: float = 15.0, interval: float = 0.3) -> bool:
    """Poll `predicate` until true or the deadline (the toggle hydrates + saves
    asynchronously: admin nav → WebdavPolicyMachine → WS-RPC → render)."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return True
        time.sleep(interval)
    return predicate()


def _webdav_enabled(client) -> bool:
    """Ground-truth `webdav_enabled` from the Admin read twin the page hydrates
    from (get_mail_config reports it, falling back to mail_enabled when unset)."""
    return bool(client.call("fauna.bridges.get_mail_config", {})["webdav_enabled"])


@pytest.mark.feature("admin-calendar-contacts-files")
def test_files_page_renders(admin_app):
    """The flat admin-files page renders its heading and the WebDAV-enable
    toggle (admin.md § Files). No port field — WebDAV rides the shared DAV
    listener admin-calendar's port input governs."""
    admin_app.admin.navigate_files()
    assert _wait(admin_app.admin.files_policy_present), (
        f"admin-files-webdav-enabled-toggle missing. error: {admin_app.error_text()!r}"
    )
    assert admin_app.driver.count("admin-files-heading") > 0, (
        f"admin-files-heading missing. error: {admin_app.error_text()!r}"
    )


@pytest.mark.feature("admin-calendar-contacts-files")
def test_webdav_enabled_toggle_round_trips(admin_app, nest_instance):
    """Flipping the deployment-wide WebDAV-enable toggle writes through the shared
    WebdavPolicyMachine → `fauna.bridges.set_webdav_enabled` → nest, proven by
    reading the Admin `get_mail_config` twin (its `webdav_enabled` field — the
    same read the toggle hydrates from). We read the initial state and assert it
    flips to the opposite, robust to whatever the catalog/mail-fallback default is."""
    admin_app.admin.navigate_files()
    assert _wait(admin_app.admin.files_policy_present), (
        f"page did not render. error: {admin_app.error_text()!r}"
    )

    client = _admin_client(nest_instance)
    with client:
        initial = _webdav_enabled(client)

        # Flip it via the UI (the rendered toggle reflects `initial`, so a click
        # dispatches set_webdav_enabled(not initial)).
        admin_app.admin.toggle_webdav_enabled()

        # Ground truth after: get_mail_config reflects the persisted flipped state.
        assert _wait(lambda: _webdav_enabled(client) == (not initial)), (
            "set_webdav_enabled did not persist the flipped state "
            f"(get_mail_config.webdav_enabled still {initial}). "
            f"error: {admin_app.error_text()!r}"
        )

    assert not admin_app.error_text().strip(), (
        f"unexpected error after a valid toggle: {admin_app.error_text()!r}"
    )

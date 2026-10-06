"""admin-contacts page (admin.md § Contacts — the deployment-wide CardDAV-enable
toggle, the contacts sibling of admin-calendar's CalDAV-enable toggle;
carddav-server.md § Independent enablement, client slice 4a).

The shared `fauna_client_mail_settings::carddav_policy::CarddavPolicyMachine` is
the one orchestrator; each app lifts it over the `build_carddav_policy_machine`
UniFFI/wasm export and renders its snapshot (same consumer model as
admin-calendar — see test_admin_calendar.py's per-app lift notes).

tier_3: a real client driver against a real `fauna-nest` binary. The round-trip
test drives the toggle (flip via the UI → shared machine `set_carddav_enabled` →
nest → re-hydrate via `get_mail_config` → re-render) and asserts ground truth
over the same Admin WS-RPC `fauna.bridges.get_mail_config` twin the page hydrates
from (its `carddav_enabled` field) — not UI introspection alone. No nest work
backs this page: `set_carddav_enabled` + the MDA gate landed in
slice 1; this is the admin-UI surface. No port sibling — CardDAV rides the shared
DAV listener the admin-calendar port field governs.
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
    read path for the round-trip (mirrors test_admin_calendar)."""
    admin = nest_instance["admin"]
    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )


def _wait(predicate, timeout: float = 15.0, interval: float = 0.3) -> bool:
    """Poll `predicate` until true or the deadline (the toggle hydrates + saves
    asynchronously: admin nav → CarddavPolicyMachine → WS-RPC → render)."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return True
        time.sleep(interval)
    return predicate()


def _carddav_enabled(client) -> bool:
    """Ground-truth `carddav_enabled` from the Admin read twin the page hydrates
    from (get_mail_config reports it, falling back to mail_enabled when unset)."""
    return bool(client.call("fauna.bridges.get_mail_config", {})["carddav_enabled"])


@pytest.mark.feature("admin-calendar-contacts-files")
def test_contacts_page_renders(admin_app):
    """The flat admin-contacts page renders its heading and the CardDAV-enable
    toggle (admin.md § Contacts). No port field — CardDAV rides the shared DAV
    listener admin-calendar's port input governs."""
    admin_app.admin.navigate_contacts()
    assert _wait(admin_app.admin.contacts_policy_present), (
        f"admin-contacts-carddav-enabled-toggle missing. error: {admin_app.error_text()!r}"
    )
    assert admin_app.driver.count("admin-contacts-heading") > 0, (
        f"admin-contacts-heading missing. error: {admin_app.error_text()!r}"
    )


@pytest.mark.feature("admin-calendar-contacts-files")
def test_carddav_enabled_toggle_round_trips(admin_app, nest_instance):
    """Flipping the deployment-wide CardDAV-enable toggle writes through the shared
    CarddavPolicyMachine → `fauna.bridges.set_carddav_enabled` → nest, proven by
    reading the Admin `get_mail_config` twin (its `carddav_enabled` field — the
    same read the toggle hydrates from). We read the initial state and assert it
    flips to the opposite, robust to whatever the catalog/mail-fallback default is."""
    admin_app.admin.navigate_contacts()
    assert _wait(admin_app.admin.contacts_policy_present), (
        f"page did not render. error: {admin_app.error_text()!r}"
    )

    client = _admin_client(nest_instance)
    with client:
        initial = _carddav_enabled(client)

        # Flip it via the UI (the rendered toggle reflects `initial`, so a click
        # dispatches set_carddav_enabled(not initial)).
        admin_app.admin.toggle_carddav_enabled()

        # Ground truth after: get_mail_config reflects the persisted flipped state.
        assert _wait(lambda: _carddav_enabled(client) == (not initial)), (
            "set_carddav_enabled did not persist the flipped state "
            f"(get_mail_config.carddav_enabled still {initial}). "
            f"error: {admin_app.error_text()!r}"
        )

    assert not admin_app.error_text().strip(), (
        f"unexpected error after a valid toggle: {admin_app.error_text()!r}"
    )

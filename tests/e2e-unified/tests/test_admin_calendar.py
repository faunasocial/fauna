"""admin-calendar page (admin.md § 8 Calendar — the deployment-wide CalDAV-enable
toggle, the sibling of admin-mail's mail-enable toggle).

linux is the **lead** client for this seed; the shared
`fauna_client_mail_settings::caldav_policy::CaldavPolicyMachine` + the linux GTK
page are the prior art the other five apps lift over the
`build_caldav_policy_machine` UniFFI/wasm export. macOS + iOS have lifted it (the
shared FaunaKit `AdminCalendarView` over the `caldavPolicyMachine()` seam — one
view, both targets); windows has lifted it (the `AdminCalendarPage` /
`AdminCalendarViewModel` over the `build_caldav_policy_machine` UniFFI seam);
android has lifted it (the `AdminCalendarScreen` / `AdminCalendarVM` over the
`com.fauna.ffi.buildCaldavPolicyMachine` UniFFI seam — its `--client android`
run is host-emulator-gated). web has lifted it (the `admin/calendar/+page.svelte`
dumb-renderer over the `caldavPolicyMachine()` wasm seam).

tier_3: a real linux GTK driver against a real `fauna-nest` binary. The round-trip
test drives the toggle (flip via the in-process agent → shared machine
`set_caldav_enabled` → nest → re-hydrate via `get_mail_config` → re-render) and
asserts ground truth over the same Admin WS-RPC `fauna.bridges.get_mail_config`
twin the page hydrates from (its `caldav_enabled` field) — not UI introspection
alone. No nest work backs this page: `set_caldav_enabled` + the MDA gate already
existed; this is the admin-UI surface.
"""
import time

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from i18n.strings import S

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
    read path for the round-trip (mirrors test_admin_mail)."""
    admin = nest_instance["admin"]
    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )


def _wait(predicate, timeout: float = 15.0, interval: float = 0.3) -> bool:
    """Poll `predicate` until true or the deadline (the toggle hydrates + saves
    asynchronously: admin nav → CaldavPolicyMachine → WS-RPC → render)."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return True
        time.sleep(interval)
    return predicate()


def _caldav_enabled(client) -> bool:
    """Ground-truth `caldav_enabled` from the Admin read twin the page hydrates
    from (get_mail_config reports it, falling back to mail_enabled when unset)."""
    return bool(client.call("fauna.bridges.get_mail_config", {})["caldav_enabled"])


def _caldav_port(client) -> int:
    """Ground-truth `caldav_port` from the Admin read twin the field hydrates from
    (get_mail_config → FetchConfigReply.caldav_port, serde default 8443) — the same
    read the port field re-reads after a save."""
    return int(client.call("fauna.bridges.get_mail_config", {})["caldav_port"])


@pytest.mark.feature("admin-calendar-contacts-files")
def test_calendar_page_renders(admin_app):
    """The flat admin-calendar page renders its heading, the CalDAV-enable toggle,
    and the admin-set CalDAV-port field (input + save button; admin.md § 8)."""
    admin_app.admin.navigate_calendar()
    assert _wait(admin_app.admin.calendar_policy_present), (
        f"admin-calendar-enabled-toggle missing. error: {admin_app.error_text()!r}"
    )
    assert admin_app.driver.count("admin-calendar-heading") > 0, (
        f"admin-calendar-heading missing. error: {admin_app.error_text()!r}"
    )
    assert _wait(admin_app.admin.caldav_port_field_present), (
        "admin-calendar-caldav-port-input / -save-button missing. "
        f"error: {admin_app.error_text()!r}"
    )


@pytest.mark.feature("admin-calendar-contacts-files")
def test_caldav_port_round_trips(admin_app, nest_instance):
    """Setting the admin CalDAV port from the UI writes through the shared
    CaldavPolicyMachine → `fauna.bridges.set_caldav_port` → nest, proven by reading
    the Admin `get_mail_config` twin (its `caldav_port` field — the same read the
    field hydrates from). We read the initial port and assert it persists a new
    valid value, robust to whatever the default is."""
    admin_app.admin.navigate_calendar()
    assert _wait(admin_app.admin.caldav_port_field_present), (
        f"port field did not render. error: {admin_app.error_text()!r}"
    )

    client = _admin_client(nest_instance)
    with client:
        initial = _caldav_port(client)
        new_port = 9443 if initial != 9443 else 9444

        # Type the new port + save (dispatches set_caldav_port(new_port)).
        admin_app.admin.set_caldav_port(str(new_port))
        admin_app.admin.save_caldav_port()

        # Ground truth after: get_mail_config reflects the persisted new port.
        assert _wait(lambda: _caldav_port(client) == new_port), (
            "set_caldav_port did not persist the new port "
            f"(get_mail_config.caldav_port still {_caldav_port(client)}, wanted {new_port}). "
            f"error: {admin_app.error_text()!r}"
        )

    assert not admin_app.error_text().strip(), (
        f"unexpected error after a valid port save: {admin_app.error_text()!r}"
    )


@pytest.mark.feature("admin-calendar-contacts-files")
def test_caldav_port_invalid_surfaces_error(admin_app, nest_instance):
    """An out-of-range port (a u16 must be in [1, 65535]) is rejected client-side
    before any dispatch: the page `error-message` shows the invalid message and the
    persisted port does NOT change (mirrors the web/android reference validation)."""
    admin_app.admin.navigate_calendar()
    assert _wait(admin_app.admin.caldav_port_field_present), (
        f"port field did not render. error: {admin_app.error_text()!r}"
    )

    client = _admin_client(nest_instance)
    with client:
        before = _caldav_port(client)

        admin_app.admin.set_caldav_port("70000")  # > 65535 → invalid u16
        admin_app.admin.save_caldav_port()

        # The page-level error-message surfaces the invalid-port string. Read the
        # element directly (the build-once settings shell can shadow error_text()).
        assert _wait(
            lambda: S.admin.calendar_page.caldav_port_invalid
            in (admin_app.driver.get_text("error-message") or "")
        ), (
            "invalid-port message not surfaced on error-message. got: "
            f"{admin_app.driver.get_text('error-message')!r}"
        )

        # And no write happened — the persisted port is unchanged.
        assert _caldav_port(client) == before, (
            "an invalid port must not dispatch set_caldav_port "
            f"(persisted port changed {before} → {_caldav_port(client)})"
        )


@pytest.mark.feature("admin-calendar-contacts-files")
def test_caldav_enabled_toggle_round_trips(admin_app, nest_instance):
    """Flipping the deployment-wide CalDAV-enable toggle writes through the shared
    CaldavPolicyMachine → `fauna.bridges.set_caldav_enabled` → nest, proven by
    reading the Admin `get_mail_config` twin (its `caldav_enabled` field — the same
    read the toggle hydrates from). We read the initial state and assert it flips to
    the opposite, robust to whatever the catalog/mail-fallback default is."""
    admin_app.admin.navigate_calendar()
    assert _wait(admin_app.admin.calendar_policy_present), (
        f"page did not render. error: {admin_app.error_text()!r}"
    )

    client = _admin_client(nest_instance)
    with client:
        initial = _caldav_enabled(client)

        # Flip it via the UI (the rendered toggle reflects `initial`, so a click
        # dispatches set_caldav_enabled(not initial)).
        admin_app.admin.toggle_caldav_enabled()

        # Ground truth after: get_mail_config reflects the persisted flipped state.
        assert _wait(lambda: _caldav_enabled(client) == (not initial)), (
            "set_caldav_enabled did not persist the flipped state "
            f"(get_mail_config.caldav_enabled still {initial}). "
            f"error: {admin_app.error_text()!r}"
        )

    assert not admin_app.error_text().strip(), (
        f"unexpected error after a valid toggle: {admin_app.error_text()!r}"
    )

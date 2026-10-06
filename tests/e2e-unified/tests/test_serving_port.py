"""admin-nest serving-port field (nest/common.md § Serving ports — the
admin-set client-facing API serving port, the symmetric twin of the CalDAV
port on admin-calendar).

A port a human picks is client UI + nest state per the iron-clad config-surface
invariant — the client UI is the only user-config surface. The admin sets the nest's
client-facing API serving port from the `admin-nest` page; it is persisted via
`fauna.admin.set_serving_port` (Admin-class) and read back from
`fauna.setup.status` (`serving_port`, serde default 443 when never set).

Unlike the CalDAV port (a `fauna.bridges.*` call with the shared
`CaldavPolicyMachine`), the serving port has no shared policy machine — the
field drives the raw `fauna.admin.set_serving_port` kind over each app's
RPC surface (`fauna_client_admin::AdminClient::set_serving_port`, exposed via
`FfiAdminClient.set_serving_port` for native apps and the wasm
`adminSetServingPort` for web).

tier_3: a real client driver against a real `fauna-nest` binary. The round-trip
drives the field (type + save → `set_serving_port` → nest persists the
`serving_port` singleton → re-hydrate via `fauna.setup.status`) and asserts
ground truth over the same `fauna.setup.status` read the field hydrates from
(its `serving_port` field, read straight from the DB) — not UI introspection
alone. The persisted value reflects immediately; the nest re-binds its listener
only on the next (re)start (it cannot hot-rebind), so no in-flight client is
stranded.
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
    """An Admin WS-RPC client keyed on the nest's admin identity (mirrors
    test_admin_calendar). `fauna.setup.status` is an anonymous read callable
    over any connection; we reuse the admin client for the ground-truth read."""
    admin = nest_instance["admin"]
    return WsRpcAdminClient(
        nest_instance["url"],
        actor_id=bytes(admin["signing_key"].verify_key),
        signing_key=bytes(admin["signing_key"]),
    )


def _wait(predicate, timeout: float = 15.0, interval: float = 0.3) -> bool:
    """Poll `predicate` until true or the deadline (the field hydrates + saves
    asynchronously: admin nav → set_serving_port → setup.status → render)."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        if predicate():
            return True
        time.sleep(interval)
    return predicate()


def _serving_port(client) -> int:
    """Ground-truth `serving_port` from `fauna.setup.status` (read straight from
    the DB singleton, serde default 443) — the same read the field hydrates from
    and re-reads after a save."""
    return int(client.call("fauna.setup.status", {})["serving_port"])


@pytest.mark.feature("admin-nest")
def test_serving_port_field_renders(admin_app):
    """The admin-nest page renders its heading and the admin-set serving-port
    field (input + save button; nest/common.md § Serving ports)."""
    admin_app.admin.navigate_nest()
    assert admin_app.driver.count("admin-nest-heading") > 0, (
        f"admin-nest-heading missing. error: {admin_app.error_text()!r}"
    )
    assert _wait(admin_app.admin.serving_port_field_present), (
        "admin-nest-serving-port-input / -save-button missing. "
        f"error: {admin_app.error_text()!r}"
    )


@pytest.mark.feature("admin-nest")
def test_serving_port_round_trips(admin_app, nest_instance):
    """Setting the admin serving port from the UI writes through
    `fauna.admin.set_serving_port` → nest, proven by reading the
    `fauna.setup.status` twin (its `serving_port` field — the same read the field
    hydrates from). We read the initial port and assert it persists a new valid
    value, then restore the nest's bind port so the singleton can be re-applied
    on a later in-place restart without bricking the shared session nest (see the
    `finally`)."""
    admin_app.admin.navigate_nest()
    assert _wait(admin_app.admin.serving_port_field_present), (
        f"serving-port field did not render. error: {admin_app.error_text()!r}"
    )

    client = _admin_client(nest_instance)
    with client:
        initial = _serving_port(client)
        new_port = 9443 if initial != 9443 else 9444

        # Type the new port + save (dispatches set_serving_port(new_port)).
        admin_app.admin.set_serving_port(str(new_port))
        admin_app.admin.save_serving_port()

        try:
            # Ground truth after: setup.status reflects the persisted new port.
            assert _wait(lambda: _serving_port(client) == new_port), (
                "set_serving_port did not persist the new port "
                f"(setup.status.serving_port still {_serving_port(client)}, wanted {new_port}). "
                f"error: {admin_app.error_text()!r}"
            )
            assert not admin_app.error_text().strip(), (
                f"unexpected error after a valid port save: {admin_app.error_text()!r}"
            )
        finally:
            # Restore the session nest's ACTUAL bind port — NOT `initial`. On a
            # fresh session nest the serving_port singleton is UNSET, so `initial`
            # is the serde DEFAULT 443 (privileged); persisting any value other
            # than the bind port poisons the next in-place restart of the SHARED
            # session nest. This is a direct-listener harness nest (no
            # FAUNA_FRONTED_BY_ROUTER), so test_nest_flip_resilience boot-resolves
            # the persisted serving_port over --bind: 443 -> EACCES (the nest
            # exits rc=1), any other port -> the health probe on the bind port
            # misses — either way the session nest dies and every later test
            # cascades into a setup error. Restoring the bind port makes
            # apply-on-restart a no-op the nest can always bind.
            restore_port = nest_instance["port"]
            admin_app.admin.set_serving_port(str(restore_port))
            admin_app.admin.save_serving_port()
            _wait(lambda: _serving_port(client) == restore_port)


@pytest.mark.feature("admin-nest")
def test_serving_port_invalid_surfaces_error(admin_app, nest_instance):
    """An out-of-range port (a u16 must be in [1, 65535]) is rejected client-side
    before any dispatch: the page `error-message` shows the invalid message and
    the persisted port does NOT change (mirrors the CalDAV-port validation)."""
    admin_app.admin.navigate_nest()
    assert _wait(admin_app.admin.serving_port_field_present), (
        f"serving-port field did not render. error: {admin_app.error_text()!r}"
    )

    client = _admin_client(nest_instance)
    with client:
        before = _serving_port(client)

        admin_app.admin.set_serving_port("70000")  # > 65535 → invalid u16
        admin_app.admin.save_serving_port()

        # The page-level error-message surfaces the invalid-port string. Read the
        # element directly (the build-once settings shell can shadow error_text()).
        assert _wait(
            lambda: S.admin.nest_page.serving_port_invalid
            in (admin_app.driver.get_text("error-message") or "")
        ), (
            "invalid-port message not surfaced on error-message. got: "
            f"{admin_app.driver.get_text('error-message')!r}"
        )

        # And no write happened — the persisted port is unchanged.
        assert _serving_port(client) == before, (
            "an invalid port must not dispatch set_serving_port "
            f"(persisted port changed {before} → {_serving_port(client)})"
        )

"""The admin audit log on the admin Logs page
(`docs/goal/behavior/admin.md` § The admin audit log on `admin-logs`).

An admin does something the nest audits — defines a tier, through the app —
then opens Logs: the newest row of the section below the log lines names the
admin (by handle) and the action with what it acted on, and the one integrity
line says the chain checks out. The rows, the line and the check are all the
shared `fauna_client_admin::audit_log_view`, which recomputes every fetched
row's hash with the function the nest writes with.

tier_3 (full stack): a real `fauna-nest`, a real admin app over WS-RPC.
"""
import secrets

import pytest

from helpers import budgets
from helpers.admin_wire import admin_user, picker_option
from helpers.app_surface import app_name, skip_unbuilt
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = [pytest.mark.tier2, pytest.mark.tier_3]

# The apps the section is built on; the others lift it in their batched
# per-app follow-on builds.
_BUILT_APPS = {"tui", "linux", "web", "android"}


@pytest.mark.feature("admin-logs")
def test_an_admin_action_is_listed_newest_and_the_record_checks_out(
    admin_app, nest_instance
):
    """Define a tier through the app → open Logs → the newest audit row is
    that admin's `tier.create` on the new tier's name, and the chain line reads
    verified."""
    app = admin_app
    if app_name(app.driver) not in _BUILT_APPS:
        skip_unbuilt(
            app.driver,
            surface="admin-audit-section",
            detail="the admin audit log section (admin.md § The admin audit log "
            "on admin-logs) is built on tui first",
            tracked="admin.md § The admin audit log on admin-logs",
        )

    # The audited action, through the app (convention 8): define a tier.
    app.admin.navigate_settings()
    wait_until(
        lambda: app.admin.tier_definition_count() >= 1,
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: f"tier rows never loaded; error: {app.error_text()!r}",
    )
    name = f"e2e-audit-{secrets.token_hex(3)}"
    app.admin.edit_new_tier_name(name)
    for cap, value in {"inbox": "1000", "storage": "1000", "devices": "4",
                       "blob-size": "1000", "feeds": "10"}.items():
        app.admin.edit_new_tier_cap(cap, value)
    app.admin.add_tier()
    wait_until(
        lambda: name in app.admin.tier_names(),
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: f"the tier never appeared: {app.admin.tier_names()!r}; "
        f"error: {app.error_text()!r}",
    )

    who = picker_option(admin_user(nest_instance))
    expected = f"{who} — tier.create on {name}"

    app.admin.navigate_logs()
    assert app.admin.logs_page_visible(), (
        f"admin-logs page not reachable. error: {app.error_text()!r}"
    )
    rows = wait_until(
        lambda: (rows := app.admin.audit_entries()) and expected in rows[0] and rows,
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: f"newest audit row should read {expected!r}; rows: "
        f"{app.admin.audit_entries()[:5]!r} error: {app.error_text()!r}",
    )
    assert rows, "the audit section painted no rows"
    # Every row shown is checked: the line counts exactly the rows painted.
    expected_status = S.admin.logs_page.audit_verified(count=str(len(rows)))
    status = app.admin.audit_integrity_text()
    assert status == expected_status, (
        f"the chain should check out through the {len(rows)} entries shown; "
        f"got {status!r}"
    )

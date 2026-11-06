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
from helpers.admin_wire import admin_client, admin_user, picker_option
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = [pytest.mark.tier2, pytest.mark.tier_3]


@pytest.mark.feature("admin-logs")
def test_an_admin_action_is_listed_newest_and_the_record_checks_out(
    admin_app, nest_instance
):
    """Define a tier through the app → open Logs → the newest audit row is
    that admin's `tier.create` on the new tier's name, and the chain line reads
    verified."""
    app = admin_app

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


@pytest.mark.feature("admin-logs")
def test_older_audit_entries_page_in_from_the_more_button(admin_app, nest_instance):
    """Seed more audited actions than one page holds → open Logs: one page of
    `AUDIT_PAGE_SIZE` rows, the oldest seeded action not among them, the more
    button enabled → press it: the older entries page in below the newer ones,
    the oldest seeded action among them, and the integrity line counts every
    row now shown."""
    app = admin_app

    # Fixture setup, not the action under test (convention 8 (b)): one page is
    # `fauna_client_admin::AUDIT_PAGE_SIZE` (50) entries, so 26 create+delete
    # pairs — 52 audited actions, no tier left behind — push the first seeded
    # entry off the newest page whatever the shared nest recorded before.
    page_size = 50
    tag = secrets.token_hex(3)
    names = [f"e2e-page-{tag}-{i:02}" for i in range(26)]
    caps = {"max_inbox_bytes": 1000, "max_storage_bytes": 1000, "max_devices": 4,
            "max_blob_size": 1000, "max_feeds": 10}
    with admin_client(nest_instance) as client:
        for name in names:
            client.call("fauna.admin.tiers.create", {"name": name, **caps})
            client.call("fauna.admin.tiers.delete", {"name": name})

    who = picker_option(admin_user(nest_instance))
    oldest = f"{who} — tier.create on {names[0]}"
    newest = f"{who} — tier.delete on {names[-1]}"

    app.admin.navigate_logs()
    assert app.admin.logs_page_visible(), (
        f"admin-logs page not reachable. error: {app.error_text()!r}"
    )
    rows = wait_until(
        lambda: (rows := app.admin.audit_entries()) and newest in rows[0] and rows,
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: f"newest audit row should read {newest!r}; rows: "
        f"{app.admin.audit_entries()[:3]!r} error: {app.error_text()!r}",
    )
    assert len(rows) == page_size, (
        f"the first read should show one page of {page_size} entries; got {len(rows)}"
    )
    assert not any(oldest in r for r in rows), (
        f"{oldest!r} is 52 actions back and should not be on the newest page"
    )
    assert app.admin.audit_more_enabled(), (
        "with older entries unread, the more button should be enabled"
    )

    app.admin.audit_more()
    paged = wait_until(
        lambda: (rows := app.admin.audit_entries()) and len(rows) > page_size and rows,
        budgets.RPC_ROUNDTRIP_S,
        diagnose=lambda: f"older entries never paged in; "
        f"{len(app.admin.audit_entries())} rows shown; error: {app.error_text()!r}",
    )
    assert paged[:page_size] == rows, "paging in keeps the newer rows, in order, above"
    assert any(oldest in r for r in paged[page_size:]), (
        f"the older page should hold {oldest!r}; it begins "
        f"{paged[page_size:page_size + 3]!r}"
    )
    expected_status = S.admin.logs_page.audit_verified(count=str(len(paged)))
    status = app.admin.audit_integrity_text()
    assert status == expected_status, (
        f"the chain should check out through all {len(paged)} entries shown; "
        f"got {status!r}"
    )

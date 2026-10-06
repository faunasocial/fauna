"""The STANDING pending-admin-actions section on the admin Users hub
(`admin.md` § Pending admin actions; IDs proposed 2026-09-24, tui-first).

A delayed admin action — a roster grant or revoke, an admin's user deletion —
is scheduled by one admin and, until 2026-09-24, was visible to nobody else:
the nest already listed every account's pending actions for admins
(`fauna.admin.pending_actions.list`) and took approvals
(`fauna.pending_actions.approve`), but no app rendered either, so a
quorum-gated roster action expired unapproved and a co-admin who might have
vetoed it never knew. This journey proves the surface through the app's own
controls (convention 8 — the mutations are UI actions):

1. `admin-users-pending-section` is STANDING and hydrates to an honest reading
   (never asserted against the bare un-hydrated title);
2. granting the admin role from a user's row (`admin-users-make-admin-button`)
   makes an `admin-pending-action-item` row appear, its description naming the
   target, its `admin-pending-action-execute-after` saying when it would apply
   and its `admin-pending-action-approvals` saying how many approvals it still
   needs — on this one-admin nest, none: the quorum is capped at the peers who
   could give one (`pending_actions::effective_quorum`), so the grant executes
   on the delay alone instead of expiring;
3. approving one's own action is refused, and the refusal lands on the
   page-scoped `admin-users-action-error` (`admin.md` § Errors);
4. `admin-pending-action-cancel-button` is ONE CLICK and removes the row, and
   the nest's own record of the action reads `cancelled` — the sanctioned
   black-box VERIFICATION (convention 8's carve-out).

The cancel reply is the causal barrier for (4): the nest holds the grant until
`execute_after` (a day away), so nothing here is timing-dependent
(convention 14). `skip_unbuilt` stays as the standing per-app guard for the
six apps the trickle-down has not reached.
"""

import pytest

from common.auth import _authed_call
from helpers.app_surface import skip_unbuilt
from helpers.budgets import RPC_ROUNDTRIP_S
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = pytest.mark.tier_3


def _open_pending_section(admin_app):
    """Land on the Users hub with the pending section hydrated, or declare the
    surface unbuilt on this app."""
    d = admin_app.driver
    admin_app.admin.navigate_users()
    try:
        d.wait_for("admin-users-pending-section", timeout=10)
    except Exception:
        skip_unbuilt(
            d,
            surface="admin-users-pending-section",
            detail=(
                "the standing pending-admin-actions section (admin.md § Pending "
                "admin actions) is not built on this app yet"
            ),
            tracked="",
        )
    bare_title = S.admin.users_page.section_pending
    wait_until(
        lambda: admin_app.admin.pending_section_text() != bare_title,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: f"section text: {admin_app.admin.pending_section_text()!r}",
    )


@pytest.mark.feature("admin-users")
def test_a_scheduled_grant_is_listed_and_cancellable_from_the_admin_console(
    admin_app, nest_instance
):
    from conftest import _make_user

    d = admin_app.driver
    target = _make_user(nest_instance)
    actor_hex = target["actor_id_hex"]

    _open_pending_section(admin_app)
    assert admin_app.admin.pending_action_row_index(actor_hex) == -1, (
        "no pending action may name this fresh user before the grant"
    )

    # (2) Schedule the grant from the target's own row.
    wait_until(
        lambda: admin_app.admin.has_user_row(actor_hex),
        RPC_ROUNDTRIP_S,
        diagnose=lambda: f"user ids: {admin_app.admin.user_actor_ids()!r}",
    )
    admin_app.admin.make_admin(admin_app.admin.user_row_index(actor_hex))
    assert not admin_app.admin.users_action_error_text(), (
        f"granting admin should schedule cleanly: "
        f"{admin_app.admin.users_action_error_text()!r}"
    )
    index = wait_until(
        lambda: (
            admin_app.admin.pending_action_row_index(actor_hex) + 1
            if admin_app.admin.pending_action_row_index(actor_hex) >= 0
            else None
        ),
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"pending rows: {admin_app.admin.pending_action_descriptions()!r}; "
            f"section: {admin_app.admin.pending_section_text()!r}; "
            f"error: {admin_app.admin.users_action_error_text()!r}"
        ),
    ) - 1
    assert admin_app.admin.pending_action_execute_after(index), (
        "the row must say when the grant would apply"
    )
    approvals = admin_app.admin.pending_action_approvals(index)
    assert approvals == S.admin.users_page.pending_approvals(given="0", needed="0"), (
        "on a one-admin nest no peer can approve, so the grant must need no "
        f"approvals rather than expire unapproved; the row reads {approvals!r}"
    )

    # (3) Self-approval is refused, on the page's own error line.
    admin_app.admin.approve_pending_action(index)
    wait_until(
        lambda: admin_app.admin.users_action_error_text(),
        RPC_ROUNDTRIP_S,
        diagnose=lambda: "approving one's own action must be refused and say so",
    )

    # (4) One click cancels; the nest's record agrees.
    action_id = _pending_action_id_for(nest_instance, actor_hex)
    admin_app.admin.cancel_pending_action(index)
    wait_until(
        lambda: admin_app.admin.pending_action_row_index(actor_hex) == -1,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"pending rows: {admin_app.admin.pending_action_descriptions()!r}; "
            f"error: {admin_app.admin.users_action_error_text()!r}"
        ),
    )
    detail = _authed_call(
        nest_instance["url"],
        nest_instance["admin"]["signing_key"],
        "fauna.pending_actions.get",
        {"id": action_id},
    )
    assert detail["status"] == "cancelled", detail
    assert not d.count("error-message") or not d.get_text("error-message"), (
        "no app-wide error may leak from a page-scoped act"
    )


def _pending_action_id_for(nest_instance, target_hex: str) -> int:
    """The id of the still-pending roster grant naming ``target_hex`` — read off
    the admin's cross-account list, so the UI's cancel can be verified against
    the nest's own record rather than against the UI's own re-read."""
    listed = _authed_call(
        nest_instance["url"],
        nest_instance["admin"]["signing_key"],
        "fauna.admin.pending_actions.list",
        {},
    )
    for row in listed["actions"]:
        if row["status"] == "pending" and row.get("target") == target_hex:
            return row["id"]
    raise AssertionError(
        f"no pending action names {target_hex[:12]}…: {listed['actions']!r}"
    )

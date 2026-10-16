"""The STANDING pending-admin-actions section on the admin Users hub
(`admin.md` § Pending admin actions; IDs user-approved 2026-09-24, tui-first,
trickled down to the other six apps 2026-10-10).

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

A second journey runs on a TWO-admin nest of its own, the app signed in as
the co-admin: the claimer schedules a grant beside the app (setup), and the
co-admin's Users hub names the claimer as its scheduler ("by <short id>") and
approves it with ONE click — the nest's own record then carries the co-admin's
approval (`admin.md` § Pending admin actions; `admin-users` outcome 40).

The cancel reply is the causal barrier for (4): the nest holds the grant until
`execute_after` (a day away), so nothing here is timing-dependent
(convention 14). All seven apps build the section, so the journey carries no
`skip_unbuilt` guard.
"""

import urllib.request

import pytest

from common import create_actor_and_register
from common.auth import _authed_call
from helpers.budgets import RPC_ROUNDTRIP_S
from helpers.waiting import wait_until
from i18n.strings import S

pytestmark = pytest.mark.tier_3


def _open_pending_section(admin_app):
    """Land on the Users hub with the pending section hydrated."""
    d = admin_app.driver
    admin_app.admin.navigate_users()
    d.wait_for("admin-users-pending-section", timeout=10)
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


# ── a co-admin approves the claimer's action in one click ────────────────────


@pytest.fixture
def two_admin_nest(request, nest_mode, tmp_path_factory):
    """A dedicated nest with a SECOND admin — never the shared ``nest_instance``,
    whose one-admin roster the journey above reads ("0 of 0 approvals").

    The grant is a day-delayed pending action; on a one-admin nest it needs no
    approval (`pending_actions::effective_quorum`), so it is brought due through
    the `test-hooks` endpoint ``POST /api/v1/test/pending_actions/run_due`` —
    never a wait (convention 14). Mirrors ``_two_admins`` in
    ``tests/api/test_admin_roster.py``."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(request, nest_mode, tmp_path_factory, "two-admin-nest")
    try:
        first_key = nest["admin"]["signing_key"]
        second = _member(nest)
        grant = _authed_call(
            nest["url"], first_key, "fauna.admin.admins.add",
            {"actor_id": bytes.fromhex(second["actor_id_hex"])},
        )
        assert grant["status"] == "pending", grant
        _run_due(nest)
        admins = {
            bytes(a["actor_id"]).hex()
            for a in _authed_call(nest["url"], first_key, "fauna.admin.admins.list", {})["admins"]
        }
        assert second["actor_id_hex"] in admins, f"the co-admin grant did not run: {admins!r}"
        nest["second_admin"] = second
        yield nest
    finally:
        cleanup()


@pytest.fixture
def two_admin_spa_url(static_dir, two_admin_nest):
    """Function-scoped SPA proxy → ``two_admin_nest`` (the session ``spa_url``
    only proxies ``nest_instance``). Mirrors ``bridges_pending_spa_url``."""
    from conftest import _serve_spa_proxy

    url, server = _serve_spa_proxy(static_dir, two_admin_nest["url"])
    yield url
    server.shutdown()


@pytest.fixture
def co_admin_app(request, app, two_admin_nest):
    """The app signed in as the nest's SECOND admin — `_login_admin_as` handed
    a view of the nest whose admin key is the co-admin's (``admin_app`` signs
    in only the claimer)."""
    from conftest import _login_admin_as

    as_second = {
        **two_admin_nest,
        "admin": {**two_admin_nest["admin"],
                  "signing_key": two_admin_nest["second_admin"]["signing_key"]},
    }
    _login_admin_as(app, request, as_second,
                    spa_url_fixture="two_admin_spa_url", fixture_name="co_admin_app")
    yield app


@pytest.mark.feature("admin-users")
def test_a_co_admin_sees_who_scheduled_an_action_and_approves_it_in_one_click(
    co_admin_app, two_admin_nest
):
    nest = two_admin_nest
    admin = co_admin_app.admin
    first_key = nest["admin"]["signing_key"]
    first_hex = bytes(first_key.verify_key).hex()
    second_hex = nest["second_admin"]["actor_id_hex"]

    # Setup beside the app: the claimer schedules a grant naming a fresh member.
    target_hex = _member(nest)["actor_id_hex"]
    scheduled = _authed_call(
        nest["url"], first_key, "fauna.admin.admins.add",
        {"actor_id": bytes.fromhex(target_hex)},
    )
    assert scheduled["status"] == "pending", scheduled
    action_id = scheduled["pending_action_id"]

    # The co-admin's Users hub lists it, naming the claimer as its scheduler
    # (`fauna_core::format::short_id` — twelve hex and an ellipsis).
    _open_pending_section(co_admin_app)
    index = wait_until(
        lambda: (
            admin.pending_action_row_index(target_hex) + 1
            if admin.pending_action_row_index(target_hex) >= 0
            else None
        ),
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"pending rows: {admin.pending_action_descriptions()!r}; "
            f"section: {admin.pending_section_text()!r}"
        ),
    ) - 1
    description = admin.pending_action_descriptions()[index]
    by_first = S.admin.users_page.pending_by(who=f"{first_hex[:12]}\u2026")
    assert by_first in description, (
        f"the row must name the claimer as its scheduler ({by_first!r}): {description!r}"
    )
    approvals = admin.pending_action_approvals(index)
    assert approvals == S.admin.users_page.pending_approvals(given="0", needed="1"), (
        f"with two admins the claimer's grant needs the co-admin's approval: {approvals!r}"
    )

    # One click approves; the row and the nest's record agree.
    admin.approve_pending_action(index)
    approved = S.admin.users_page.pending_approvals(given="1", needed="1")
    wait_until(
        lambda: admin.pending_action_row_index(target_hex) >= 0
        and admin.pending_action_approvals(admin.pending_action_row_index(target_hex)) == approved,
        RPC_ROUNDTRIP_S,
        diagnose=lambda: (
            f"pending rows: {admin.pending_action_descriptions()!r}; "
            f"error: {admin.users_action_error_text()!r}"
        ),
    )
    assert not admin.users_action_error_text(), (
        f"a co-admin's approval must land cleanly: {admin.users_action_error_text()!r}"
    )
    # `fauna.pending_actions.get` answers the creator only — read as the claimer.
    detail = _authed_call(nest["url"], first_key, "fauna.pending_actions.get", {"id": action_id})
    assert detail["status"] == "pending", detail
    assert detail["approvals"] == [second_hex], (
        f"the nest must record exactly the co-admin's approval: {detail!r}"
    )


def _member(nest) -> dict:
    return create_actor_and_register(nest["port"], base_url=nest["url"],
                                     admin_signing_key=nest["admin"]["signing_key"])


def _run_due(nest) -> dict:
    """Run the production pending-action executor once (`test-hooks`)."""
    import json

    req = urllib.request.Request(f"{nest['url']}/api/v1/test/pending_actions/run_due",
                                 data=b"", method="POST")
    with urllib.request.urlopen(req, timeout=30.0) as resp:
        assert resp.status == 200, f"run_due hook returned {resp.status}"
        return json.loads(resp.read())

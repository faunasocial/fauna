"""Who hears a pending admin action, over the wire
(`notifications.md` § Security notices → *Pending actions*, ruled 2026-09-24).

An admin's action against another account (`fauna.admin.users.delete`) used to
ring its creator only: the person it named learned of it if it executed, and
the cancel the authorization matrix already granted them pointed at nothing —
their own `fauna.pending_actions.list` was keyed on the creator. This pins the
target's half of the ruling end-to-end on the production kinds, with no app in
the loop (the app leg is `test_pending_actions.py`'s target-side journey):

* the target's list now carries the action against them;
* the target is told at scheduling — a `security.notice` row whose localized
  body is `row_security_pending_action_against_you`, naming the action's id;
* the target cancels it, and the admin who scheduled it is told who did.

The co-admin half (every other admin told at scheduling, on execution, on a
cancel and on an unapproved expiry) needs a second admin, which the 24 h
`admin.add` delay puts out of this harness's reach; it is pinned in the nest
(`pending_actions::security_notice_tests`). Nothing here waits on time: every
notice is written before the scheduling call's reply (convention 14).
"""

import pytest

from common.auth import _authed_call, port_base_url
from clients.ws_rpc_admin_client import WsRpcAdminClient
from conftest import _make_user

pytestmark = pytest.mark.tier_3

AGAINST_YOU = "notifications.row_security_pending_action_against_you"
CANCELLED = "notifications.row_security_action_cancelled"


def _security_notice_keys(nest, sk) -> list[str]:
    """The localized-body keys of ``sk``'s security notices, OLDEST first (the
    wire list is newest first; sorting on the row id gives the ring order)."""
    reply = _authed_call(nest["url"], sk, "fauna.notifications.list", {"limit": 100})
    rows = sorted(
        (r for r in reply["notifications"] if r.get("notif_type") == "security.notice"),
        key=lambda r: r["id"],
    )
    return [(row.get("body") or {}).get("key") for row in rows]


def _security_notice_args(nest, sk, key: str) -> list[dict]:
    """The args of every security notice with ``key``, newest first."""
    reply = _authed_call(nest["url"], sk, "fauna.notifications.list", {"limit": 100})
    return [
        (row.get("body") or {}).get("args") or {}
        for row in reply["notifications"]
        if row.get("notif_type") == "security.notice"
        and (row.get("body") or {}).get("key") == key
    ]


@pytest.mark.feature("admin-users")
def test_the_target_of_an_admin_deletion_is_told_and_can_cancel_it(nest_instance):
    nest = nest_instance
    admin_sk = nest["admin"]["signing_key"]
    target = _make_user(nest)
    target_sk = target["signing_key"]

    # ── The admin schedules the deletion (7-day delay; never executes here).
    with WsRpcAdminClient(
        port_base_url(nest["port"]),
        actor_id=bytes(admin_sk.verify_key),
        signing_key=bytes(admin_sk),
    ) as admin:
        scheduled = admin.call(
            "fauna.admin.users.delete",
            {"actor_id": bytes.fromhex(target["actor_id_hex"])},
        )
    assert scheduled["status"] == "pending", scheduled
    action_id = scheduled["pending_action_id"]

    # ── The target's own list carries it — the row Settings → Pending actions
    #    renders with its cancel.
    listed = _authed_call(nest["url"], target_sk, "fauna.pending_actions.list", {})
    mine = next((a for a in listed["actions"] if a["id"] == action_id), None)
    assert mine is not None, (
        f"the action against the target must be in their own list: {listed!r}"
    )
    assert mine["action_type"] == "admin.delete_user", mine

    # ── The target was told at scheduling, with the id they need to find it.
    assert _security_notice_keys(nest, target_sk) == [AGAINST_YOU], (
        "exactly one notice, the against-you one, at scheduling"
    )
    [args] = _security_notice_args(nest, target_sk, AGAINST_YOU)
    assert args.get("action_id") == str(action_id), args
    assert args.get("execute_after") == str(scheduled["execute_after"]), args

    # ── The target cancels it; the admin hears who did.
    cancelled = _authed_call(
        nest["url"], target_sk, "fauna.pending_actions.cancel", {"id": action_id}
    )
    assert cancelled["ok"] is True, cancelled
    detail = _authed_call(nest["url"], admin_sk, "fauna.pending_actions.get", {"id": action_id})
    assert detail["status"] == "cancelled", detail
    assert _security_notice_keys(nest, target_sk) == [AGAINST_YOU, CANCELLED]
    # The admin's list is the session's shared one, so look for THIS cancel by
    # what it says: the action type, and the target as the one who did it (the
    # notice names an account by handle when it has one, else by hex).
    target_labels = {target.get("handle"), target["actor_id_hex"]}
    assert any(
        args.get("action_type") == "admin.delete_user"
        and args.get("cancelled_by") in target_labels
        for args in _security_notice_args(nest, admin_sk, CANCELLED)
    ), (
        "the admin must be told the target called the deletion off; cancelled "
        f"notices: {_security_notice_args(nest, admin_sk, CANCELLED)!r}"
    )

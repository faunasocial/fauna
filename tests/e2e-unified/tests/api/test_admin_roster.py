"""Who is an admin, and how it changes — the roster witnessed at the wire
(`docs/goal/behavior/admin.md` § Pending admin actions and § Cutting a user
off — eviction and suspension; `docs/goal/behavior/notifications.md`
§ Security notices → *Pending actions*).

The nest outcomes of `docs/features/admin-users.md` that need a SECOND admin,
which an app journey cannot be — here both admins are RPC clients:

* **32** — an admin's account cannot be deleted, by another admin or by its
  owner, until the admin role is removed.
* **33** — a scheduled removal that would leave the nest with no admin does not
  run; it waits until another admin exists.
* **34** — with two admins neither changes who is an admin alone: the other
  approves a grant, a removal needs the approval of the admin being removed,
  and an action left short of its approvals lapses without effect.
* **35** — the other admins are told when an admin action is scheduled (who
  scheduled it, whom it names, when it runs, how many approvals it still needs)
  and again when it runs, is called off or lapses.
* the role-granted and role-removed halves of **36** — the member is told when
  the admin role is granted to or removed from their account.

Every roster action carries a deliberate delay (a day for a grant or a
removal), so each one is brought due through the `test-hooks` endpoint
`POST /api/v1/test/pending_actions/run_due`, which runs the production
executor once — never a wait (convention 14). Each test starts its own nest:
the roster is nest-wide state no shared nest may lose.
"""

from __future__ import annotations

import pytest
from nacl.signing import SigningKey

from clients._ws_rpc_core import RpcCallError
from clients.ws_rpc_admin_client import WsRpcAdminClient
from common import create_actor_and_register
from helpers.admin_wire import grant_admin, run_due_pending_actions

pytestmark = pytest.mark.tier_3

PENDING = "notifications.row_security_admin_action_pending"
CHANGE = "notifications.row_security_admin_change"
CANCELLED = "notifications.row_security_admin_action_cancelled"
EXPIRED = "notifications.row_security_admin_action_expired"


@pytest.fixture
def roster_nest(request, nest_mode, tmp_path_factory):
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(request, nest_mode, tmp_path_factory, "roster-nest")
    raw = nest["admin"]["signing_key"]
    sk = raw if isinstance(raw, SigningKey) else SigningKey(bytes.fromhex(raw))
    nest["first"] = {"signing_key": sk, "actor_id_hex": bytes(sk.verify_key).hex()}
    nest["first"]["handle"] = _call(nest, nest["first"], "fauna.account.get", {}).get("handle")
    yield nest
    cleanup()


# ── wire helpers ─────────────────────────────────────────────────────────────


def _member(nest) -> dict:
    return create_actor_and_register(nest["port"], base_url=nest["url"],
                                     admin_signing_key=nest["first"]["signing_key"])


def _call(nest, who: dict, kind: str, payload: dict) -> dict:
    with WsRpcAdminClient(nest["url"], actor_id=bytes.fromhex(who["actor_id_hex"]),
                          signing_key=bytes(who["signing_key"])) as ws:
        return ws.call(kind, payload)


def _id(who: dict) -> bytes:
    return bytes.fromhex(who["actor_id_hex"])


def _schedule(nest, by: dict, kind: str, target: dict) -> int:
    reply = _call(nest, by, kind, {"actor_id": _id(target)})
    assert reply["status"] == "pending", reply
    return reply["pending_action_id"]


def _approve(nest, by: dict, action_id: int) -> None:
    _call(nest, by, "fauna.pending_actions.approve", {"id": action_id})


def _run_due(nest) -> dict:
    return run_due_pending_actions(nest)


def _status(nest, by: dict, action_id: int) -> str:
    """The action's status, read by the admin ``by`` over the cross-account
    list every admin sees (`fauna.pending_actions.get` answers its creator only)."""
    actions = _call(nest, by, "fauna.admin.pending_actions.list", {})["actions"]
    row = next((a for a in actions if a["id"] == action_id), None)
    assert row is not None, f"action {action_id} missing from the admin list: {actions!r}"
    return row["status"]


def _admins(nest, by: dict) -> set[str]:
    return {bytes(a["actor_id"]).hex() for a in _call(nest, by, "fauna.admin.admins.list", {})["admins"]}


def _notices(nest, who: dict, key: str) -> list[dict]:
    """The args of every security notice ``who`` holds with ``key``."""
    rows = _call(nest, who, "fauna.notifications.list", {"limit": 200})["notifications"]
    return [
        (r.get("body") or {}).get("args") or {}
        for r in rows
        if r.get("notif_type") == "security.notice" and (r.get("body") or {}).get("key") == key
    ]


def _about(args_list: list[dict], action_type: str) -> list[dict]:
    return [a for a in args_list if a.get("action_type", a.get("change_type")) == action_type]


def _two_admins(nest) -> tuple[dict, dict]:
    """The claimer plus a second admin granted on the claimer's delay alone (a
    sole admin's grant needs no approval nobody could give)."""
    first = nest["first"]
    second = _member(nest)
    grant_admin(nest, second)
    assert _admins(nest, first) == {first["actor_id_hex"], second["actor_id_hex"]}
    return first, second


def _refusal(fn, *args) -> str:
    with pytest.raises(RpcCallError) as ei:
        fn(*args)
    return ei.value.code


# ── 32 (and 36's grant + removal): an admin's account is guarded ─────────────


@pytest.mark.feature("admin-users")
def test_an_admins_account_cannot_be_deleted_until_the_role_is_removed(roster_nest):
    nest = roster_nest
    first, second = _two_admins(nest)
    assert _about(_notices(nest, second, CHANGE), "admin.add"), (
        "the member must be told the admin role was granted to them"
    )

    code = _refusal(_call, nest, first, "fauna.admin.users.delete", {"actor_id": _id(second)})
    assert code == "fauna.admin.conflict", f"another admin's delete must be refused: {code}"
    code = _refusal(_call, nest, second, "fauna.account.delete", {})
    assert code == "fauna.account.admin_role_held", f"the owner's own delete must be refused: {code}"

    # Remove the role: with two admins the removal needs its target's consent.
    removal = _schedule(nest, first, "fauna.admin.admins.remove", second)
    _approve(nest, second, removal)
    _run_due(nest)
    assert _status(nest, first, removal) == "executed"
    assert _admins(nest, first) == {first["actor_id_hex"]}
    assert _about(_notices(nest, second, CHANGE), "admin.remove"), (
        "the member must be told the admin role was removed from them"
    )

    deletion = _call(nest, first, "fauna.admin.users.delete", {"actor_id": _id(second)})
    assert deletion["status"] == "pending", (
        f"once the role is gone the account's deletion can be scheduled: {deletion!r}"
    )


# ── 33: a removal that would empty the roster waits ──────────────────────────


@pytest.mark.feature("admin-users")
def test_a_removal_that_would_leave_no_admin_waits_until_another_admin_exists(roster_nest):
    nest = roster_nest
    first, second = _two_admins(nest)

    # Two removals, each legal while two admins stand, each consented to.
    remove_second = _schedule(nest, first, "fauna.admin.admins.remove", second)
    _approve(nest, second, remove_second)
    remove_first = _schedule(nest, second, "fauna.admin.admins.remove", first)
    _approve(nest, first, remove_first)

    _run_due(nest)
    assert _status(nest, first, remove_second) == "executed"
    assert _admins(nest, first) == {first["actor_id_hex"]}, "the nest is never left without an admin"
    assert _status(nest, first, remove_first) == "pending", (
        "the removal that would leave no admin must not run, and must wait"
    )

    third = _member(nest)
    grant = _schedule(nest, first, "fauna.admin.admins.add", third)
    # The grant and the parked removal are both due now; whichever order the
    # executor takes them in, a second pass leaves nothing parked. Read as the
    # new admin — the first may no longer be one.
    _run_due(nest)
    _run_due(nest)
    assert _status(nest, third, grant) == "executed"
    assert _status(nest, third, remove_first) == "executed", (
        "once another admin exists the waiting removal runs"
    )
    assert _admins(nest, third) == {third["actor_id_hex"]}


# ── 34: two admins change the roster only together ───────────────────────────


@pytest.mark.feature("admin-users")
def test_with_two_admins_no_roster_change_is_unilateral_and_a_short_action_lapses(roster_nest):
    nest = roster_nest
    first, second = _two_admins(nest)

    # A removal needs its target's approval; the scheduler's own is refused.
    removal = _schedule(nest, first, "fauna.admin.admins.remove", second)
    _refusal(_approve, nest, first, removal)
    _run_due(nest)
    assert _status(nest, first, removal) == "expired"
    assert second["actor_id_hex"] in _admins(nest, first), "an unapproved removal lapses without effect"

    # A grant short of the other admin's approval lapses too.
    third = _member(nest)
    grant = _schedule(nest, first, "fauna.admin.admins.add", third)
    _run_due(nest)
    assert _status(nest, first, grant) == "expired"
    assert third["actor_id_hex"] not in _admins(nest, first)

    # The other admin's approval is what lets a grant run.
    grant = _schedule(nest, first, "fauna.admin.admins.add", third)
    _approve(nest, second, grant)
    _run_due(nest)
    assert _status(nest, first, grant) == "executed"
    assert third["actor_id_hex"] in _admins(nest, first)


# ── 35: the other admins hear every step ─────────────────────────────────────


@pytest.mark.feature("admin-users")
def test_the_other_admins_are_told_at_scheduling_and_when_it_runs_is_called_off_or_lapses(
        roster_nest):
    nest = roster_nest
    first, second = _two_admins(nest)
    first_labels = {first["actor_id_hex"], first.get("handle")}

    # Scheduled: who, whom, when, how many approvals still needed.
    third = _member(nest)
    grant = _schedule(nest, first, "fauna.admin.admins.add", third)
    told = [a for a in _notices(nest, second, PENDING) if a.get("action_id") == str(grant)]
    assert told, "the other admin must be told when the grant is scheduled"
    detail = _call(nest, first, "fauna.pending_actions.get", {"id": grant})
    assert told[0].get("by") in first_labels, told[0]
    assert told[0].get("target") in {third["actor_id_hex"], third.get("handle")}, told[0]
    assert told[0].get("execute_after") == str(detail["execute_after"]), told[0]
    assert told[0].get("approvals_needed") == "1", told[0]

    # Runs.
    _approve(nest, second, grant)
    _run_due(nest)
    assert any(a.get("target") in {third["actor_id_hex"], third.get("handle")}
               for a in _about(_notices(nest, second, CHANGE), "admin.add")), (
        "the other admin must be told when the grant runs"
    )

    # Called off.
    fourth = _member(nest)
    called_off = _schedule(nest, first, "fauna.admin.admins.add", fourth)
    _call(nest, first, "fauna.pending_actions.cancel", {"id": called_off})
    assert any(a.get("action_id") == str(called_off) for a in _notices(nest, second, CANCELLED)), (
        "the other admin must be told when the action is called off"
    )

    # Lapses.
    lapsing = _schedule(nest, first, "fauna.admin.admins.add", fourth)
    _run_due(nest)
    assert _status(nest, first, lapsing) == "expired"
    assert any(a.get("action_id") == str(lapsing) for a in _notices(nest, second, EXPIRED)), (
        "the other admin must be told when the action lapses"
    )


# ── admin-nest 24: no rotation while an admin removal is pending ─────────────


def _rotation_chain(nest) -> list:
    from clients.ws_rpc_anon_client import WsRpcAnonClient

    with WsRpcAnonClient(nest["url"]) as anon:
        return anon.call("fauna.auth.rotation_chain", {}).get("chain") or []


@pytest.mark.feature("admin-nest")
def test_the_identity_rotation_is_refused_while_an_admin_removal_is_pending(roster_nest):
    """`docs/features/admin-nest.md` outcome 24 (`nest/box-recovery.md`
    § Deployment-seed rotation → *Ordering rule — rotate only into a clean
    roster*). While the removal of the second admin waits, the rotation is
    refused and the box keeps its identity; once the removal has run, the same
    request rotates."""
    import secrets

    nest = roster_nest
    first, second = _two_admins(nest)
    removal = _schedule(nest, first, "fauna.admin.admins.remove", second)
    assert _status(nest, first, removal) == "pending"

    rotate = {"new_seed": secrets.token_bytes(32).hex()}
    _refusal(_call, nest, first, "fauna.admin.deployment_seed.rotate", rotate)
    assert _rotation_chain(nest) == [], "a refused rotation must leave the identity as it was"

    _approve(nest, second, removal)
    _run_due(nest)
    assert _status(nest, first, removal) == "executed"
    reply = _call(nest, first, "fauna.admin.deployment_seed.rotate", rotate)
    assert reply.get("already_rotated") is False and reply.get("seq") == 1, (
        f"with the removal done the same rotation goes through: {reply!r}"
    )

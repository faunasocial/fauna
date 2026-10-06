"""E2E tests: Admin auth system (claim-admin, am-i-admin, admin management).

The personal account / admin-UI-gate HTTP twins (``GET /api/v1/am-i-admin``)
were deleted by the WS-RPC-everywhere migration
(tracked internally, Track B1) and replaced with the
``fauna.account.am_i_admin`` WS-RPC kind. These tests drive that kind via the
generic ``clients.ws_rpc_admin_client.WsRpcAdminClient`` (Admin name is
historical — a User-class actor's keypair makes it a User-class client). Wire
contract verified against ``libs/fauna-protocol/src/account.rs::AmIAdminReply``
+ ``bins/fauna-nest/src/account_handlers.rs::am_i_admin_handler`` (2026-05-24):
``fauna.account.am_i_admin`` takes ``{}`` and replies ``{"admin": bool}``;
caller class ``User | Admin`` (any registered actor).

Admin management likewise migrated to WS-RPC — the ``/admin/api/admins`` HTTP
twins (GET/POST/DELETE) were deleted in the WS-RPC-everywhere rip-out → the
Admin-class kinds ``fauna.admin.admins.{list,add,remove}``
(``bins/fauna-nest/src/admin_ws_handlers.rs``).

Admin add/remove are **deliberately delayed, quorum-gated security actions**
(``bins/fauna-nest/src/pending_actions.rs``): ``fauna.admin.admins.add``
creates an ``AdminAdd`` pending action (24h delay; nominal quorum 1, stored as
the number of peer admins who could approve — 0 on a one-admin nest) and returns
``AdminPendingActionReply`` ``{pending_action_id, execute_after,
status:"pending"}`` — the new admin is NOT immediately added.
``fauna.admin.admins.remove`` likewise schedules an ``AdminRemove`` (24h delay,
quorum=2), refusing to schedule removal of the last superadmin synchronously
(``fauna.admin.conflict``). This is intentional product behavior (api-layers.md
§ Destructive operations are delayed), not a bug. The eventual EXECUTION path
(action reaches "executed" after quorum+time, or expires without quorum) is
covered by the Rust integration test ``bins/fauna-nest/tests/pending_actions.rs``
(which calls ``execute_ready_actions`` directly, bypassing the wall-clock wait) —
so these e2e tests verify the API SURFACE, not eventual execution.
"""

import time

from common import (
    CLAIM_CODE,
    create_actor_and_register,
)
from common.auth import port_base_url

from clients.ws_rpc_admin_client import WsRpcAdminClient
from clients.ws_rpc_anon_client import RpcCallError, WsRpcAnonClient

import pytest

pytestmark = pytest.mark.tier_3


@pytest.fixture()
def admin_auth_nest(request, nest_mode, tmp_path_factory):
    """A fresh claimed nest for one admin-auth test.

    Zero start options — each of these tests wants a claimed nest and nothing
    else, so the family routes through the mode provider instead of compiling
    a binary the run may not be using.
    """
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "admin-auth-nest")
    yield nest
    cleanup()


def am_i_admin(port, actor) -> bool:
    """Call ``fauna.account.am_i_admin`` over WS-RPC for ``actor``.

    ``actor`` is a ``common.auth`` actor dict (``actor_id_bytes`` +
    ``signing_key``). Returns the bool from the ``{"admin": bool}`` reply.
    Replaces the deleted ``GET /api/v1/am-i-admin`` HTTP twin.
    """
    client = WsRpcAdminClient(
        port_base_url(port),
        actor_id=bytes(actor["signing_key"].verify_key),
        signing_key=bytes(actor["signing_key"]),
    )
    with client:
        reply = client.call("fauna.account.am_i_admin", {})
    return bool(reply["admin"])


@pytest.mark.feature("claim-a-fresh-nest")
def test_claim_admin(admin_auth_nest):
    """Claiming admin works; the claim code is single-use (410 on retry)."""
    nest = admin_auth_nest
    port = nest["port"]
    admin_token = nest["admin"]["token"]
    assert admin_token, "Expected admin token from start_nest"

    # The claimed admin sees admin=true via fauna.account.am_i_admin
    # (the WS-RPC replacement for GET /api/v1/am-i-admin).
    assert am_i_admin(port, nest["admin"]) is True

    # Try to claim again with the same code — the claim-code file is gone, so
    # the WS kind rejects with `fauna.auth.already_claimed` (the old HTTP
    # twin's 410 Gone). claim-admin is now the pre-identity WS-RPC kind
    # `fauna.auth.claim_admin` — its `POST /api/v1/claim-admin` HTTP twin was
    # removed in S4d (tracked internally).
    from nacl.signing import SigningKey
    sk = SigningKey.generate()
    actor_id_hex = bytes(sk.verify_key).hex()
    timestamp = int(time.time())
    from common.sig_domain import claim_admin_signed_message

    msg = claim_admin_signed_message(bytes(sk.verify_key), timestamp)
    sig = sk.sign(msg).signature

    with WsRpcAnonClient(port_base_url(port)) as anon:
        try:
            anon.call(
                "fauna.auth.claim_admin",
                {
                    "claim_code": CLAIM_CODE,
                    "actor_id": actor_id_hex,
                    "signature": sig.hex(),
                    "timestamp": timestamp,
                    # handle is required by the wire (the claim type-promotion
                    # makes a handle-less request fail to decode → malformed);
                    # include a valid one so the request reaches the
                    # already-claimed check rather than being rejected earlier.
                    "handle": "admin",
                },
            )
            assert False, "Expected already_claimed on second claim attempt"
        except RpcCallError as e:
            assert e.code == "fauna.auth.already_claimed", (
                f"Expected fauna.auth.already_claimed, got {e.code}"
            )

@pytest.mark.feature("admin-dashboard")
def test_am_i_admin_non_admin(admin_auth_nest):
    """A regular (non-admin) user gets admin=false from fauna.account.am_i_admin."""
    nest = admin_auth_nest
    port = nest["port"]
    # Create a regular user (CallerClass::User).
    user = create_actor_and_register(
        port, admin_signing_key=nest["admin"]["signing_key"]
    )

    assert am_i_admin(port, user) is False

@pytest.mark.feature("admin-users")
def test_admin_management(admin_auth_nest):
    """Admin add/remove are delayed, quorum-gated pending actions — all WS-RPC.

    Verifies the API SURFACE of the new model (not eventual execution, which
    ``bins/fauna-nest/tests/pending_actions.rs`` covers by calling
    ``execute_ready_actions`` directly) over the ``fauna.admin.admins.*`` kinds
    that replaced the deleted ``/admin/api/admins`` HTTP twins:

    * ``fauna.admin.admins.list`` lists current admins.
    * ``fauna.admin.admins.add`` schedules an ``AdminAdd`` pending action
      (24h delay; nominal quorum 1, stored as the count of peers who could
      approve — 0 on this one-admin nest) — returns ``{pending_action_id,
      execute_after, status:"pending"}``, and the second user is NOT yet admin.
    * The scheduling actor sees the action via ``fauna.pending_actions.list``
      / ``fauna.pending_actions.get`` (B20 WS-RPC kinds).
    * The action can be cancelled via ``fauna.pending_actions.cancel``.
    * ``fauna.admin.admins.remove`` enforces the roster floor synchronously:
      with a single superadmin (the only count reachable from e2e) every removal
      is rejected with ``fauna.admin.conflict`` before any pending action is
      scheduled.
    """
    nest = admin_auth_nest
    port = nest["port"]
    admin_actor = nest["admin"]

    admin_client = WsRpcAdminClient(
        port_base_url(port),
        actor_id=bytes(admin_actor["signing_key"].verify_key),
        signing_key=bytes(admin_actor["signing_key"]),
    )
    with admin_client:
        # List admins — should have exactly 1.
        listed = admin_client.call("fauna.admin.admins.list", {})
        assert len(listed["admins"]) == 1, (
            f"Expected 1 admin, got {listed['admins']}"
        )

        # Create a second user; scheduling admin-add does NOT make them admin yet.
        second_user = create_actor_and_register(
            port, admin_signing_key=nest["admin"]["signing_key"]
        )
        second_actor_id = second_user["actor_id_hex"]

        # fauna.admin.admins.add schedules an AdminAdd pending action. The wire
        # `actor_id` rides as raw 32 bytes (ByteBuf), not the twin's hex.
        add_data = admin_client.call(
            "fauna.admin.admins.add",
            {"actor_id": bytes.fromhex(second_actor_id)},
        )
        assert add_data["status"] == "pending", (
            f"Expected status=pending, got {add_data}"
        )
        action_id = add_data["pending_action_id"]
        assert isinstance(action_id, int), f"Expected int pending_action_id, got {add_data}"
        # 24h AdminAdd delay → execute_after is comfortably in the future
        # (epoch seconds; create_pending_action sets now + 24*3600).
        now = int(time.time())
        assert add_data["execute_after"] > now, (
            f"execute_after {add_data['execute_after']} should be > now {now}"
        )
        assert add_data["execute_after"] >= now + 23 * 3600, (
            f"AdminAdd should delay ~24h; execute_after={add_data['execute_after']}, now={now}"
        )

        # Admin roster is unchanged — the action is only scheduled, not executed.
        listed = admin_client.call("fauna.admin.admins.list", {})
        assert len(listed["admins"]) == 1, (
            f"Expected 1 admin (add not yet executed), got {listed['admins']}"
        )

        # The scheduling actor (admin) sees the action via the B20 WS-RPC kinds.
        pa = admin_client.call("fauna.pending_actions.list", {})
        ids = [a["id"] for a in pa["actions"]]
        assert action_id in ids, (
            f"Scheduled AdminAdd {action_id} not in actor's pending list {ids}"
        )
        scheduled = next(a for a in pa["actions"] if a["id"] == action_id)
        assert scheduled["action_type"] == "admin.add", scheduled
        assert scheduled["status"] == "pending", scheduled
        # The nominal AdminAdd quorum is 1, but a quorum counts the admins who
        # could actually give an approval (self-approval is refused), and this
        # nest has one admin — so the stored requirement is capped at 0 and the
        # grant executes on its delay alone instead of expiring unapproved
        # (`pending_actions::effective_quorum`; nest/common.md § Pending Actions
        # System). Before 2026-09-24 this read 1 and the ratified co-admin
        # instrument could never complete on a one-admin nest.
        assert scheduled["requires_quorum"] == 0, scheduled

        # fauna.pending_actions.get returns the per-id detail.
        detail = admin_client.call("fauna.pending_actions.get", {"id": action_id})
        assert detail["id"] == action_id
        assert detail["action_type"] == "admin.add"
        assert detail["target"] == second_actor_id
        assert detail["status"] == "pending"

        # Cancel it via fauna.pending_actions.cancel (the creator may cancel).
        cancel_reply = admin_client.call(
            "fauna.pending_actions.cancel", {"id": action_id}
        )
        assert cancel_reply["ok"] is True

        # After cancellation the action is no longer pending.
        detail2 = admin_client.call("fauna.pending_actions.get", {"id": action_id})
        assert detail2["status"] != "pending", (
            f"Expected cancelled action, got status={detail2['status']}"
        )

        # fauna.admin.admins.remove — the superadmin floor is enforced
        # *synchronously* before scheduling any AdminRemove pending action
        # (admin_ws_handlers::admins_remove_handler); the writer re-refuses
        # at execution, which is the floor's authoritative line
        # (behavior/admin.md § the last-superadmin guard, ratified
        # 2026-08-14).
        #
        # ⚠ 2026-08-20: this used to
        # loop over BOTH targets expecting a refusal for each, and failed
        # `DID NOT RAISE` on the first. It was the TEST that was wrong, and
        # its own comment already said so — it described the rule correctly
        # as "target-aware `can_remove_admin`: the last superadmin is
        # refused" and then asserted the strictly stronger "regardless of
        # target". The floor is **target-aware by design**: `can_remove_admin`
        # (db/admin.rs) consults the target's role and returns true for
        # anything that is not a superadmin, and the goal doc guards exactly
        # "the last superadmin", not every removal. Asserting the stronger
        # rule would have made a genuine narrowing of the floor invisible.
        #
        # So the two targets are asserted apart, which is also what makes the
        # pair meaningful: the guard must discriminate, not blanket-refuse.
        #
        # A non-admin target is NOT protected by the floor: the removal is
        # legal and schedules the pending action. (Executing it is a no-op —
        # there is no role to drop — and the eventual-execution path is the
        # Rust pending_actions.rs test's, not an e2e wall-clock concern.)
        removal = admin_client.call(
            "fauna.admin.admins.remove",
            {"actor_id": bytes.fromhex(second_actor_id)},
        )
        assert removal["status"] == "pending", (
            "removing a non-superadmin must schedule an AdminRemove pending "
            f"action rather than trip the floor; got {removal}"
        )

        # The sole superadmin IS protected — synchronously, before any action
        # is scheduled. This is the assertion that would catch the floor
        # being lost.
        with pytest.raises(RpcCallError) as ei:
            admin_client.call(
                "fauna.admin.admins.remove",
                {"actor_id": bytes.fromhex(nest["admin"]["actor_id_hex"])},
            )
        assert ei.value.code == "fauna.admin.conflict", (
            "removing the last superadmin must refuse fauna.admin.conflict "
            f"(an off-box brick otherwise); got {ei.value.code}"
        )

    # The second user is correctly NOT yet admin (am-i-admin over WS-RPC, on
    # its own connection).
    assert am_i_admin(port, second_user) is False

def test_setup_status_claimed(admin_auth_nest):
    """After admin is claimed, ``fauna.setup.status`` reports claimed=true.

    Migrated off the deprecated ``GET /api/v1/setup-status`` HTTP twin (deleted
    in S4c2) onto the anonymous WS-RPC kind via :class:`WsRpcAnonClient`.
    """
    port = admin_auth_nest["port"]
    with WsRpcAnonClient(port_base_url(port)) as anon:
        data = anon.call("fauna.setup.status", {})
    assert data.get("claimed") is True, (
        f"Expected claimed=true after admin claim, got {data}"
    )

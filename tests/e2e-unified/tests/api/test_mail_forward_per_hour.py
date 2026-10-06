"""Tier 3: a user reads and sets their own hourly forwarding limit over the real
WS-RPC wire — `fauna.bridges.{get,set}_forward_per_hour`, the doors an app's
mail settings call (`docs/goal/behavior/mail-forwarding.md` § Per-account
forward rate-limit: default 100 an hour, settable lower than the admin ceiling,
never higher).

That the limit set here is the one the forward rate cap enforces is witnessed
end to end by `test_mail_bridge_forward_floors.py::
test_forward_over_the_hourly_cap_waits_and_goes_out_later`, which sets it
through the same door. Every assertion reads a reply the nest wrote for this
call (convention 14).
"""

import pytest

from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient
from common.auth import create_actor_and_register

pytestmark = pytest.mark.tier_3


def _user_ws(nest_instance, user):
    return WsRpcAdminClient(nest_instance["url"], actor_id=user["actor_id_bytes"],
                            signing_key=bytes(user["signing_key"]))


def test_a_user_sets_their_own_hourly_forward_limit_within_the_ceiling(nest_instance):
    """A fresh account reads the default limit and the ceiling it may not pass;
    a limit it sets reads back; zero and anything above the ceiling are refused
    and leave the limit as it was; another user's limit is untouched."""
    admin_sk = nest_instance["admin"]["signing_key"]
    user = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_sk)
    other = create_actor_and_register(nest_instance["port"], admin_signing_key=admin_sk)

    with _user_ws(nest_instance, user) as ws:
        start = ws.call("fauna.bridges.get_forward_per_hour", {})
        assert start["forward_per_hour"] == 100, f"the default limit is 100 an hour; got {start!r}"
        ceiling = start["forward_per_hour_ceiling"]
        assert ceiling >= 100, f"the ceiling must admit the default; got {start!r}"

        ws.call("fauna.bridges.set_forward_per_hour", {"forward_per_hour": 7})
        assert ws.call("fauna.bridges.get_forward_per_hour", {})["forward_per_hour"] == 7

        ws.call("fauna.bridges.set_forward_per_hour", {"forward_per_hour": ceiling})
        assert ws.call("fauna.bridges.get_forward_per_hour", {})["forward_per_hour"] == ceiling

        for refused in (0, ceiling + 1):
            with pytest.raises(RpcCallError) as e:
                ws.call("fauna.bridges.set_forward_per_hour", {"forward_per_hour": refused})
            assert e.value.code == "fauna.protocol.malformed", (
                f"a limit of {refused} must be refused; got {e.value.code!r}")
            assert ws.call("fauna.bridges.get_forward_per_hour", {})["forward_per_hour"] == ceiling, (
                "a refused limit must leave the previous one in place")

    with _user_ws(nest_instance, other) as ws:
        assert ws.call("fauna.bridges.get_forward_per_hour", {})["forward_per_hour"] == 100, (
            "one user's limit is theirs alone")

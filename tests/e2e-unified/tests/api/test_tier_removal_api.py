"""An admin removes a tier nobody is on — on a real ``fauna-nest`` binary,
through ``fauna.admin.tiers.delete``, the Settings page's removal door.

Owner doc: ``docs/goal/behavior/admin.md`` § 3. Settings (*Removing a tier*):
the nest refuses the removal with ``fauna.admin.conflict`` while a member, an
invite code or a membership link still names the tier, and always for ``free``
(the tier the nest itself names); once nothing names the tier it goes. The
membership-link arms and the atomic check are pinned in-process
(``conformance_admin.rs``'s ``tiers_delete_*``); this file is the end-to-end
witness the feature catalog can cite.

⚠ The test works on a tier of its own, named per run, on the shared session
nest — other tests' actors sit on ``free`` and on their own tiers.
"""

import secrets

import pytest

from clients._ws_rpc_core import RpcCallError
from clients.ws_rpc_admin_client import WsRpcAdminClient
from common.auth import create_actor_and_register, ensure_tier, set_user_tier

pytestmark = pytest.mark.tier_3


def _admin_ws(nest) -> WsRpcAdminClient:
    admin = nest["admin"]
    return WsRpcAdminClient(
        nest["url"], actor_id=admin["actor_id_bytes"], signing_key=bytes(admin["signing_key"]))


def _tier_names(ws) -> set[str]:
    return {t["name"] for t in ws.call("fauna.admin.tiers.list", {})["tiers"]}


def _refused(ws, name: str) -> str:
    with pytest.raises(RpcCallError) as refused:
        ws.call("fauna.admin.tiers.delete", {"name": name})
    return refused.value.code


@pytest.mark.feature("admin-tiers")
def test_a_tier_is_removable_only_once_nothing_names_it(nest_instance):
    """A tier a member is on, and then one an invite code admits at, is refused
    and stays listed; with the member moved off and the code deleted the same
    removal succeeds and the tier leaves the list. ``free`` is always refused
    and an unknown name is not found."""
    nest = nest_instance
    sk = nest["admin"]["signing_key"]
    tier = f"e2e-removal-{secrets.token_hex(4)}"
    ensure_tier(nest["port"], admin_signing_key=sk, name=tier, base_url=nest["url"])
    member = create_actor_and_register(nest["port"], admin_signing_key=sk, base_url=nest["url"])
    set_user_tier(nest["port"], member["actor_id_hex"], tier, admin_signing_key=sk,
                  base_url=nest["url"])

    with _admin_ws(nest) as ws:
        assert _refused(ws, tier) == "fauna.admin.conflict"
        assert tier in _tier_names(ws), "a refused removal leaves the tier listed"

        set_user_tier(nest["port"], member["actor_id_hex"], "free", admin_signing_key=sk,
                      base_url=nest["url"])
        code = ws.call("fauna.admin.invite_codes.create", {"tier": tier, "uses": 1})["code"]
        assert _refused(ws, tier) == "fauna.admin.conflict"
        assert tier in _tier_names(ws)

        ws.call("fauna.admin.invite_codes.delete", {"code": code})
        assert ws.call("fauna.admin.tiers.delete", {"name": tier})["ok"] is True
        assert tier not in _tier_names(ws), "the removed tier leaves the list"

        assert _refused(ws, "free") == "fauna.admin.conflict"
        assert _refused(ws, tier) == "fauna.admin.not_found"

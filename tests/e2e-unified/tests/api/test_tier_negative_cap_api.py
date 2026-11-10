"""The nest refuses a negative tier cap at the door — on a real ``fauna-nest``
binary, through ``fauna.admin.tiers.create`` and ``fauna.admin.tiers.update``.

Owner docs: ``docs/goal/behavior/value-formatting.md`` § Tier cap validation
(the nest refuses a negative cap too, and the app clamp is not what makes that
unnecessary: an older app is a supported peer) and
``docs/goal/architecture/api-layers.md`` § Track C / C2 (the
``fauna.admin.invalid_params`` contract). ``0`` stays valid at the door — the
refusal is ``< 0``, never ``< 1``.

⚠ The test works on tiers of its own, named per run, on the shared session nest.
"""

import secrets

import pytest

from clients._ws_rpc_core import RpcCallError
from clients.ws_rpc_admin_client import WsRpcAdminClient

pytestmark = pytest.mark.tier_3

_CAPS = ("max_inbox_bytes", "max_storage_bytes", "max_devices", "max_blob_size", "max_feeds")


def _admin_ws(nest) -> WsRpcAdminClient:
    admin = nest["admin"]
    return WsRpcAdminClient(
        nest["url"], actor_id=admin["actor_id_bytes"], signing_key=bytes(admin["signing_key"]))


def _tiers(ws) -> dict[str, dict]:
    return {t["name"]: t for t in ws.call("fauna.admin.tiers.list", {})["tiers"]}


def _request(name: str, **overrides: int) -> dict:
    # Valid, small caps (the device cap has a ceiling of its own at the door).
    caps = {cap: 1000 for cap in _CAPS}
    caps["max_devices"] = 2
    caps.update(overrides)
    return {"name": name, **caps}


def _refusal(ws, kind: str, params: dict) -> str:
    with pytest.raises(RpcCallError) as refused:
        ws.call(kind, params)
    return refused.value.code


@pytest.mark.feature("admin-tiers")
def test_the_nest_refuses_a_negative_tier_cap_and_stores_nothing(nest_instance):
    """A tier created with any one negative cap is refused with
    ``fauna.admin.invalid_params`` and never listed; an update giving an
    existing tier a negative cap is refused the same way and the tier keeps
    its caps; a cap of ``0`` is accepted on both doors."""
    suffix = secrets.token_hex(4)
    with _admin_ws(nest_instance) as ws:
        for cap in _CAPS:
            name = f"e2e-neg-{cap.removeprefix('max_')}-{suffix}"
            code = _refusal(ws, "fauna.admin.tiers.create", _request(name, **{cap: -1}))
            assert code == "fauna.admin.invalid_params", (
                f"create with a negative {cap} answered {code!r}")
            assert name not in _tiers(ws), f"a refused create stored tier {name!r}"

        kept = f"e2e-neg-kept-{suffix}"
        ws.call("fauna.admin.tiers.create", _request(kept, max_feeds=0))
        assert _tiers(ws)[kept]["max_feeds"] == 0, "a cap of 0 is valid at the door"
        before = _tiers(ws)[kept]
        for cap in _CAPS:
            code = _refusal(ws, "fauna.admin.tiers.update", _request(kept, **{cap: -5}))
            assert code == "fauna.admin.invalid_params", (
                f"update with a negative {cap} answered {code!r}")
        assert _tiers(ws)[kept] == before, (
            f"a refused update changed the tier: {before!r} -> {_tiers(ws)[kept]!r}")

        ws.call("fauna.admin.tiers.update", _request(kept, max_devices=0))
        assert _tiers(ws)[kept]["max_devices"] == 0, "an update to 0 is valid at the door"

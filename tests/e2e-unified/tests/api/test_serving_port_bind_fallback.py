"""tier_3 API: a serving port the nest cannot bind never takes it down
(`docs/goal/architecture/nest/common.md` § Serving ports → *Privileged-port
safety*).

`docs/features/admin-nest.md` outcome 17, on a real `fauna-nest` binary that
really stops and starts. The admin picks a serving port this test is holding
open, so the nest's next bind of it fails `AddrInUse` — the same failure an
unprivileged nest meets on a port under 1024, without needing a privilege the
harness lacks. The serving port applies on restart, so the nest is restarted;
it must come up on the port it had, and the admin must be able to pick another.

Direct-listener only: behind the SNI router (docker, live) the serving port is
refused outright (`serving_port_fronted`), so there is nothing to fall back
from — hence `standalone_only`. The nest is dedicated: a restart stops it.
Process safety: `common.nest.restart_nest` stops the fixture's own process
handle only; the held port is this test's own socket.
"""

from __future__ import annotations

import socket

import pytest

from clients.ws_rpc_admin_client import WsRpcAdminClient
from clients.ws_rpc_anon_client import WsRpcAnonClient

pytestmark = [pytest.mark.tier_3, pytest.mark.standalone_only]


@pytest.fixture
def port_nest(request, nest_mode, tmp_path_factory):
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(request, nest_mode, tmp_path_factory, "serving-port-nest")
    try:
        yield nest
    finally:
        cleanup()


def _admin_call(nest, kind: str, payload: dict) -> dict:
    sk = nest["admin"]["signing_key"]
    with WsRpcAdminClient(nest["url"], actor_id=bytes(sk.verify_key), signing_key=bytes(sk)) as ws:
        return ws.call(kind, payload)


def _nest_id(nest) -> str:
    with WsRpcAnonClient(nest["url"]) as anon:
        return anon.call("fauna.nest.info", {})["nest_id"]


@pytest.mark.feature("admin-nest")
def test_an_unbindable_serving_port_leaves_the_nest_reachable_on_the_port_it_had(port_nest):
    from common.nest import restart_nest
    from tests.api import ws_api

    nest = port_nest
    identity = _nest_id(nest)
    held = socket.socket(socket.AF_INET, socket.SOCK_STREAM)
    try:
        held.bind(("0.0.0.0", 0))
        held.listen(1)
        taken = held.getsockname()[1]
        _admin_call(nest, "fauna.admin.set_serving_port", {"port": taken})

        ws_api.close_all_ws()
        restart_nest(nest, graceful=True)

        assert _nest_id(nest) == identity, (
            f"after choosing port {taken}, which it cannot bind, the nest must still answer "
            f"at {nest['url']}"
        )
    finally:
        held.close()

    reply = _admin_call(nest, "fauna.admin.set_serving_port", {"port": nest["port"]})
    assert reply is not None, "the admin can pick another serving port on the nest that stayed up"

"""A nest that runs no relay offers none — ``p2p.md`` § Implementation status
today (the shipping-image row: "a nest with no such name, or with no relay
binary beside it, advertises none and clients use the nest-mediated
fallback"), on a real ``fauna-nest`` binary.

A tier_3 nest is the exact deployment that sentence names: a bare binary with
no relay sidecar beside it. The nest's relay advertisement is gated on a live
fact — a relay sidecar holding its channel to the nest
(``discovery_core::relay_sidecar_connected``) — so what this proves is that the
anonymous ``fauna.nest.info`` a device reads at sign-in carries neither the
``relay`` capability nor a relay address, and that the ordinary path through the
nest is what is left: the reply is a normal one.

The nest is given a public name of its own on purpose. Without one the URL
would be absent for a second reason (no name to serve under — outcome 3, which
the image tier witnesses with a relay that really runs), and the test could
pass on the wrong one.
"""

import pytest

from clients.ws_rpc_anon_client import WsRpcAnonClient

pytestmark = pytest.mark.tier_3

#: A public DNS name (`fauna_core::resolve::is_public_dns_name`), so the only
#: thing between this nest and a relay advertisement is the missing relay.
NEST_DOMAIN = "relayless.example"


@pytest.fixture(scope="module")
def named_nest(request, nest_mode, tmp_path_factory):
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "relayless", claim_domain=NEST_DOMAIN,
    )
    try:
        yield nest
    finally:
        cleanup()


@pytest.mark.feature("nest-relays-peer-traffic")
def test_a_nest_running_no_relay_advertises_none(named_nest):
    with WsRpcAnonClient(named_nest["url"]) as anon:
        info = anon.call("fauna.nest.info", {})

    # `web_serving_domain` follows the claimed identity domain — the same
    # `handle_domain()` the relay address would be minted from.
    assert info.get("web_serving_domain") == NEST_DOMAIN, (
        f"the nest must hold its public name, or a missing relay address proves "
        f"nothing about the missing relay: {info!r}"
    )
    caps = info.get("capabilities") or []
    assert caps, f"nest.info must carry its capability set: {info!r}"
    assert "relay" not in caps, (
        f"a nest with no relay beside it advertised the relay capability: {caps}"
    )
    assert not info.get("iroh_relay_url"), (
        f"a nest with no relay beside it advertised a relay address: "
        f"{info.get('iroh_relay_url')!r}"
    )

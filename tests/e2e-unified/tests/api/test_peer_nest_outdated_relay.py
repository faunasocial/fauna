"""tier_3 two-binary proof: a cross-nest action the other nest refuses because
it is too old is answered, by the user's own nest, with the typed and
localized ``fauna.federation.peer_nest_outdated`` — never the raw refusal.

`docs/goal/architecture/version-compatibility.md` § Dimension 5 — Federation
across nest versions: an older peer's federation allowlist refuses a kind it
predates with ``fauna.protocol.unauthenticated`` before any handler runs, and
every relay maps that to ``peer_nest_outdated``
(``bins/fauna-nest/src/rpc_errors.rs::map_peer_relay_error``).

The older peer is made a state, not a version hunt: nest B's ``rpc-hold`` test
hook answers ``fauna.federation.channel.append`` with exactly the old
allowlist's refusal (``refuse_rpc(..., as_="unauthenticated")``,
``bins/fauna-nest/src/rpc_hold_test_hook.rs``). Nest A is untouched: the user
sends to a channel homed on B (``fauna.conversations.channel.send_remote``),
A dials B over the real federation channel, and the answer the user receives
is A's. B's refusal counter proves the request crossed the wire and was
turned away there, not refused locally on A.

Standalone-nest only, like every ``rpc-hold`` test: the hook is compiled into
the ``test-hooks`` e2e build and into no release artifact (convention 15).
"""

import os

import pytest

from clients._ws_rpc_core import RpcCallError
from common.auth import create_actor_and_register, port_base_url
from helpers.rpc_hold import (
    refuse_rpc,
    release_rpc_hold,
    rpc_hold_status,
)

from tests.api import conv_api

pytestmark = pytest.mark.tier_3

FED_APPEND = "fauna.federation.channel.append"


@pytest.fixture()
def two_nests(request, nest_mode, tmp_path_factory):
    """Two dedicated nests: A (the user's) and B (the channel's home)."""
    from conftest import _start_dedicated_nest

    nest_a, cleanup_a = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "peer-outdated-a")
    try:
        nest_b, cleanup_b = _start_dedicated_nest(
            request, nest_mode, tmp_path_factory, "peer-outdated-b")
        try:
            yield nest_a, nest_b
        finally:
            cleanup_b()
    finally:
        cleanup_a()


@pytest.mark.feature("upgrades-never-lose-data")
def test_send_to_a_too_old_home_nest_is_answered_peer_nest_outdated(two_nests):
    nest_a, nest_b = two_nests
    alice = create_actor_and_register(
        nest_a["port"], admin_signing_key=nest_a["admin"]["signing_key"]
    )
    client = conv_api._client_for(port_base_url(nest_a["port"]), alice)

    before = rpc_hold_status(nest_b["port"], FED_APPEND).get("refused", 0)
    refuse_rpc(nest_b["port"], FED_APPEND, as_="unauthenticated")
    try:
        with pytest.raises(RpcCallError) as refused:
            client.call(
                "fauna.conversations.channel.send_remote",
                {
                    "channel_id": os.urandom(32).hex(),
                    "nest_url": port_base_url(nest_b["port"]),
                    # B refuses before any handler reads it, as an old peer's
                    # allowlist does, so the bytes are never inspected.
                    "envelope": os.urandom(64),
                    "attachment_refs": [],
                },
            )
        after = rpc_hold_status(nest_b["port"], FED_APPEND).get("refused", 0)
    finally:
        release_rpc_hold(nest_b["port"], FED_APPEND)

    assert after == before + 1, (
        f"nest B turned away {after - before} append(s), want exactly 1 — the "
        f"send never crossed to the home nest, so A's answer proves nothing: "
        f"{refused.value}"
    )
    err = refused.value
    assert err.code == "fauna.federation.peer_nest_outdated", (
        f"an old home nest's refusal must reach the user as peer_nest_outdated, got {err}"
    )
    assert err.message_key == "error.federation.peer_nest_outdated", err

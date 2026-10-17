"""The update channel as nest state — ``nest/update-channel.md`` § The channel as
nest state, on a real ``fauna-nest`` binary.

* **The seed.** A nest booted with ``FAUNA_IMAGE_TAG=test`` follows Test while no
  admin has chosen: ``/api/v1/health`` reports ``update_channel: "test"`` and
  ``fauna.admin.update_channel.get`` answers ``channel: "test"``. A tag that is
  no channel (``sha-…``) seeds none — the box is *pinned*: ``null`` on health,
  no ``channel`` on the read.
* **The admin's choice wins and survives.** ``set { dev }`` is reflected on the
  read and on health at once, and again after a restart that keeps the
  ``test`` seed in the environment — the row, not the seed, is in force.
* **An older nest.** A kind the router never registered answers
  ``fauna.protocol.unknown_kind`` and the connection stays usable — the answer
  an app reads as "this nest predates the choice", never a failed read.
* A non-admin can neither read nor set it, and an unknown channel id is refused.

``channel_head_version``: an e2e nest resolves it only against the harness
registry ``FAUNA_E2E_UPDATE_REGISTRY_ORIGIN`` names (`helpers/oci_registry.py`).
The two nests above name none, so their read leaves it absent — never reaching
the real registry; ``head_nest`` names one whose ``test`` tag carries version
``9.9.9``, and its read answers that, then answers without it once the registry
has no head or is gone — the read never fails on the head.
"""

import json
import urllib.request

import pytest

from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient

pytestmark = pytest.mark.tier_3


def _start(request, nest_mode, tmp_path_factory, label: str, tag: str):
    from conftest import _start_dedicated_nest

    return _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, label,
        extra_env={"FAUNA_IMAGE_TAG": tag},
    )


@pytest.fixture(scope="module")
def test_channel_nest(request, nest_mode, tmp_path_factory):
    """A dedicated nest installed from the ``test`` tag, which these tests
    restart — never the session nest."""
    nest, cleanup = _start(request, nest_mode, tmp_path_factory,
                           "update-channel-test", "test")
    try:
        yield nest
    finally:
        cleanup()


@pytest.fixture(scope="module")
def pinned_nest(request, nest_mode, tmp_path_factory):
    """A dedicated nest installed from a commit tag — no channel."""
    nest, cleanup = _start(request, nest_mode, tmp_path_factory,
                           "update-channel-pinned", "sha-abc1234")
    try:
        yield nest
    finally:
        cleanup()


@pytest.fixture(scope="module")
def head_registry():
    """The harness registry: its ``test`` tag at version ``9.9.9``, no ``dev``
    tag at all."""
    from helpers.oci_registry import OciRegistry

    with OciRegistry() as registry:
        registry.synthetic("test", "9.9.9")
        yield registry


@pytest.fixture(scope="module")
def head_nest(request, nest_mode, tmp_path_factory, head_registry):
    """A dedicated nest on the ``test`` seed that reads its channel's head from
    the harness registry."""
    from conftest import _start_dedicated_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "update-channel-head",
        extra_env={
            "FAUNA_IMAGE_TAG": "test",
            "FAUNA_E2E_UPDATE_REGISTRY_ORIGIN": head_registry.origin,
        },
    )
    try:
        yield nest
    finally:
        cleanup()


def _client(nest, key=None) -> WsRpcAdminClient:
    key = key or nest["admin"]["signing_key"]
    return WsRpcAdminClient(
        nest["url"], actor_id=bytes(key.verify_key), signing_key=bytes(key)
    )


def _health(nest) -> dict:
    with urllib.request.urlopen(nest["url"].rstrip("/") + "/api/v1/health") as resp:
        return json.loads(resp.read())


def _get(nest) -> dict:
    with _client(nest) as admin:
        return admin.call("fauna.admin.update_channel.get", {})


def _set(nest, channel: str) -> dict:
    with _client(nest) as admin:
        return admin.call("fauna.admin.update_channel.set", {"channel": channel})


@pytest.mark.feature("admin-nest")
def test_the_seed_is_followed_until_the_admin_chooses_and_the_choice_survives_a_restart(
    test_channel_nest,
):
    from common.nest import restart_nest

    nest = test_channel_nest
    health = _health(nest)
    assert health["update_channel"] == "test", health
    reply = _get(nest)
    assert reply["channel"] == "test", reply
    assert reply["running_version"] == health["version"], (reply, health)
    assert reply.get("channel_head_version") is None, reply

    assert _set(nest, "dev")["ok"] is True
    assert _get(nest)["channel"] == "dev"
    assert _health(nest)["update_channel"] == "dev", "health must follow the row at once"

    restart_nest(nest, graceful=True)
    assert _health(nest)["update_channel"] == "dev", (
        "the admin's row must win over the FAUNA_IMAGE_TAG=test seed after a restart"
    )
    assert _get(nest)["channel"] == "dev"


@pytest.mark.feature("admin-nest")
def test_a_non_channel_tag_leaves_the_box_pinned(pinned_nest):
    health = _health(pinned_nest)
    assert "update_channel" in health and health["update_channel"] is None, health
    reply = _get(pinned_nest)
    assert reply.get("channel") is None, reply
    assert reply["running_version"] == health["version"]


@pytest.mark.feature("admin-nest")
def test_an_unknown_kind_is_the_predates_the_choice_answer(pinned_nest):
    with _client(pinned_nest) as admin:
        with pytest.raises(RpcCallError) as err:
            admin.call("fauna.admin.update_channel.not_yet_minted", {})
        assert err.value.code == "fauna.protocol.unknown_kind", err.value
        # The connection survives the miss: the read an app retries works.
        assert "running_version" in admin.call("fauna.admin.update_channel.get", {})


@pytest.mark.feature("admin-nest")
def test_a_non_admin_cannot_read_or_set_it_and_an_unknown_channel_is_refused(pinned_nest):
    from common.auth import create_actor_and_register

    member = create_actor_and_register(
        pinned_nest["port"], admin_signing_key=pinned_nest["admin"]["signing_key"]
    )
    with _client(pinned_nest, member["signing_key"]) as user:
        for kind, params in (
            ("fauna.admin.update_channel.get", {}),
            ("fauna.admin.update_channel.set", {"channel": "dev"}),
        ):
            with pytest.raises(RpcCallError):
                user.call(kind, params)
    with _client(pinned_nest) as admin:
        with pytest.raises(RpcCallError):
            admin.call("fauna.admin.update_channel.set", {"channel": "latest"})
    assert _health(pinned_nest)["update_channel"] is None, (
        "a refused set must not have moved the box off its pin"
    )


@pytest.mark.feature("admin-nest")
@pytest.mark.standalone_only
def test_the_channel_head_version_is_read_from_the_registry_and_absent_without_it(
    head_nest, head_registry,
):
    """``standalone_only``: the registry is a loopback listener this process
    owns, named to the nest through a ``test-hooks`` seam — only a locally
    spawned test-hooks nest reads it."""
    reply = _get(head_nest)
    assert reply["channel"] == "test", reply
    assert reply.get("channel_head_version") == "9.9.9", (
        f"the read must carry the Test head's version label: {reply}; "
        f"the registry saw {head_registry.requests}"
    )
    assert "HEAD /v2/faunasocial/nest/manifests/test" in head_registry.requests, (
        head_registry.requests
    )

    # A channel with no head in the registry: absent, and the read answers.
    assert _set(head_nest, "dev")["ok"] is True
    reply = _get(head_nest)
    assert reply["channel"] == "dev", reply
    assert reply.get("channel_head_version") is None, reply

    # The registry gone: back on Test (a channel the five-minute cache no
    # longer holds), the head cannot be had — absent, and the read succeeds.
    head_registry.stop()
    assert _set(head_nest, "test")["ok"] is True
    reply = _get(head_nest)
    assert reply["channel"] == "test", reply
    assert reply["running_version"] == _health(head_nest)["version"], reply
    assert reply.get("channel_head_version") is None, (
        f"an unreachable registry must leave the head absent: {reply}"
    )

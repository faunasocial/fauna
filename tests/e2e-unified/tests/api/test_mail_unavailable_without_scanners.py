"""tier_3 API: a bare nest whose data dir carries a hatch declaring no content
scanners reports it and refuses the mail enable — the Docker-free seam the
claim-time and Mail-page rows reuse.

The Docker image's entrypoint writes ``content_scanners`` onto
``{data_dir}/operator-hatch.toml``; here the test seeds that file itself and
restarts the nest in place, since the nest reads it once at boot. The tier_4 twin
(``platform/docker/test_mail_unavailable_without_scanners_docker.py``) proves the
entrypoint writes it. Owner: ``docs/goal/behavior/mail-bridge-lifecycle.md``
§ Default-off on first claim → *A box with no content scanners cannot enable
mail*.
"""

import os

import pytest

from clients.ws_rpc_admin_client import RpcCallError, WsRpcAdminClient
from clients.ws_rpc_anon_client import WsRpcAnonClient
from common.auth import port_base_url

pytestmark = pytest.mark.tier_3


@pytest.fixture()
def no_scanner_nest(request, nest_mode, tmp_path_factory):
    from conftest import _start_dedicated_nest
    from helpers import nest_mode as nest_mode_mod
    from helpers.app_surface import skip_environment

    if not nest_mode_mod.builds_local_nest(nest_mode):
        skip_environment(
            "the hatch is seeded into a locally-spawned nest's data dir; an image "
            "nest's entrypoint regenerates it on every start (the tier_4 twin "
            "covers that path)"
        )
    from common.nest import start_nest_in_place, stop_nest

    nest, cleanup = _start_dedicated_nest(
        request, nest_mode, tmp_path_factory, "no-scanner-nest"
    )
    try:
        stop_nest(nest, graceful=True)
        with open(os.path.join(nest["tmp_dir"], "operator-hatch.toml"), "w") as f:
            f.write('content_scanners = "absent"\n')
        start_nest_in_place(nest)
        yield nest
    finally:
        cleanup()


def _status(nest) -> dict:
    with WsRpcAnonClient(port_base_url(nest["port"])) as anon:
        return anon.call("fauna.setup.status", {})


def _admin(nest) -> WsRpcAdminClient:
    sk = nest["admin"]["signing_key"]
    return WsRpcAdminClient(
        port_base_url(nest["port"]), actor_id=bytes(sk.verify_key), signing_key=bytes(sk)
    )


def test_a_box_declaring_no_scanners_refuses_the_mail_enable(no_scanner_nest):
    nest = no_scanner_nest
    status = _status(nest)
    assert status["node_mode"] == "public", status
    assert status.get("content_scanners") == "absent", status

    with _admin(nest) as admin:
        with pytest.raises(RpcCallError) as refused:
            admin.call("fauna.bridges.set_mail_enabled", {"enabled": True})
        assert refused.value.code == "fauna.bridges.mail_unavailable_here", refused.value
        admin.call("fauna.bridges.set_mail_enabled", {"enabled": False})

    assert _status(nest)["email_enabled"] is False
    assert not os.path.exists(os.path.join(nest["tmp_dir"], "imap-enabled")), (
        "the refusal comes before the flag write"
    )


def test_a_nest_with_no_hatch_reports_unknown(nest_instance, nest_mode):
    """Outside the image nothing declared anything: ``unknown``, and the
    enable stays offered (today's behaviour). An image nest reports what its
    entrypoint declared instead."""
    from helpers import nest_mode as nest_mode_mod

    status = _status(nest_instance)
    if nest_mode_mod.builds_local_nest(nest_mode):
        assert status.get("content_scanners") == "unknown", status
    else:
        assert status.get("content_scanners") in ("wired", "absent"), status

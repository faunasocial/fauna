"""tier_4: a nest image started WITHOUT content scanners cannot enable mail.

The wizard's social-only VPS, the home-relay box and a single-container run wire
no clamd/rspamd sidecar, and the image ships no scanner of its own — so the MTA's
fail-closed scan gate would 451 every inbound message. The entrypoint declares
that on the topology hatch (``content_scanners = "absent"``), the nest reads it at
boot, and on the public axis refuses ``fauna.bridges.set_mail_enabled(true)`` with
the typed ``fauna.bridges.mail_unavailable_here`` — nothing persisted, no flag, no
DKIM, the MTA s6 service down. A container started with the scanner env reports
``wired`` and enables as before.

Owner: ``docs/goal/behavior/mail-bridge-lifecycle.md`` § Default-off on first
claim → *A box with no content scanners cannot enable mail* (the observable
paragraph names these assertions).
"""

import subprocess

import pytest

from clients._ws_rpc_core import RpcCallError

from .helpers import (
    admin_ws,
    bridge_diag,
    claim_admin_api,
    create_network,
    docker_build,
    find_free_port,
    flag_present,
    get_repo_root,
    is_commanded_up,
    remove_container,
    remove_network,
    start_container,
    start_container_with_ports,
    start_fake_scanner_sidecars,
    svstat,
    wait_for_health,
)

try:
    subprocess.run(["docker", "info"], capture_output=True, timeout=10)
    HAS_DOCKER = True
except Exception:
    HAS_DOCKER = False

pytestmark = [
    pytest.mark.skipif(not HAS_DOCKER, reason="Docker not available"),
    pytest.mark.tier_4,
]

MTA = "fauna-mail-bridge-mta"


def _setup_status(port: int) -> dict:
    from clients.ws_rpc_anon_client import WsRpcAnonClient

    with WsRpcAnonClient(f"https://127.0.0.1:{port}") as anon:
        return anon.call("fauna.setup.status", {})


def _claimed(name: str, port: int, code: str) -> dict:
    wait_for_health(port, name)
    admin = claim_admin_api(port, code, handle="admin")
    return {"name": name, "port": port, "url": f"https://127.0.0.1:{port}", "admin": admin}


def test_no_scanners_public_box_refuses_mail_enable():
    docker_build(get_repo_root())
    port = find_free_port()
    name = f"fauna-noscan-{port}"
    code = "NOSCAN1"
    try:
        # The public axis (FAUNA_MODE defaults public), no FAUNA_CLAMD_ADDR /
        # FAUNA_RSPAMD_URL — the social-only VPS shape.
        start_container(name, port, env={"FAUNA_CLAIM_CODE": code})
        nest = _claimed(name, port, code)

        status = _setup_status(port)
        assert status["node_mode"] == "public", status
        assert status.get("content_scanners") == "absent", (
            "the entrypoint must declare content_scanners = \"absent\" on the hatch "
            f"and the nest report it; setup.status = {status!r}"
        )

        with admin_ws(nest) as admin:
            with pytest.raises(RpcCallError) as refused:
                admin.call("fauna.bridges.set_mail_enabled", {"enabled": True})
            assert refused.value.code == "fauna.bridges.mail_unavailable_here", refused.value
            # A disable is never refused.
            admin.call("fauna.bridges.set_mail_enabled", {"enabled": False})

        status = _setup_status(port)
        assert status["email_enabled"] is False, status
        assert not status.get("dkim_records"), (
            f"a refused enable must publish no DKIM record; got {status.get('dkim_records')!r}"
        )
        assert not flag_present(name), "/data/imap-enabled must never be written"
        mta = svstat(name, MTA)
        assert not is_commanded_up(mta), (
            f"the MTA must stay down on a box without scanners; svstat={mta!r}\n"
            f"bridge diag:\n{bridge_diag(name)}"
        )
    finally:
        remove_container(name)


def test_scanner_wired_box_still_enables_mail():
    docker_build(get_repo_root())
    suffix = find_free_port()
    network = f"fauna-scanwired-net-{suffix}"
    clamd_name = f"fauna-scanwired-clamd-{suffix}"
    rspamd_name = f"fauna-scanwired-rspamd-{suffix}"
    fakes_dir = str(get_repo_root() / "tests" / "e2e-unified" / "fakes")
    name = None
    code = "SCANW1"
    create_network(network)
    try:
        scan = start_fake_scanner_sidecars(
            network, fakes_dir, clamd_name=clamd_name, rspamd_name=rspamd_name
        )
        port = find_free_port()
        name = f"fauna-scanwired-{port}"
        start_container_with_ports(
            name,
            {443: port},
            env={
                "FAUNA_CLAIM_CODE": code,
                "FAUNA_PORT": "3000",
                "FAUNA_CLAMD_ADDR": scan["clamd_addr"],
                "FAUNA_RSPAMD_URL": scan["rspamd_url"],
            },
            network=network,
        )
        nest = _claimed(name, port, code)
        assert _setup_status(port).get("content_scanners") == "wired"

        with admin_ws(nest) as admin:
            admin.call("fauna.bridges.set_mail_enabled", {"enabled": True})
        assert _setup_status(port)["email_enabled"] is True
        assert flag_present(name), "the enable must still write /data/imap-enabled"
    finally:
        if name:
            remove_container(name)
        remove_container(clamd_name)
        remove_container(rspamd_name)
        remove_network(network)

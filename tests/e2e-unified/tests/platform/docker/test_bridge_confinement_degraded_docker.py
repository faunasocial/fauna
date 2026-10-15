"""tier_4: a mail helper started without its sandbox shows up as a warning in the
admin's log — ``security.md`` § Confinement self-probe, on the real image.

Each mail bridge measures its own confinement at startup, from inside whatever
the deployment wrapped it in (``cmd/fauna-mail-bridge/main.go``, the
``confinement.Probe`` call), and a degraded result is emitted on the sidecar log
plane as the catalogued ``confinement_degraded`` warning
(``internal/logplane/catalogue.go::ConfinementDegraded``), which the nest stamps
``<role>:confinement_degraded`` and serves on ``fauna.admin.logs`` — the Logs
page an admin reads on a box nobody can SSH into.

**The misconfiguration is reproduced, not simulated.** The shipped run script
starts the MTA as ``s6-setuidgid fauna-mta fauna-sandbox bridge -- fauna-mail-bridge
…``; the honest mistake the probe exists to catch is a deployment that drops the
``fauna-sandbox`` wrapper (a hand-rolled compose, say). The test makes exactly
that edit to the live service directory, restarts the one service, and reads the
admin's log. The control is the same box before the edit: a sandboxed bridge
that came up serving emitted no such warning.

The route itself (a real bridge's event over its real WS into the real nest's
admin log) is the one ``test_sidecar_log_plane_docker.py`` proves for ``ready``.
"""

from __future__ import annotations

import subprocess

import pytest

from helpers.waiting import wait_until

from .helpers import (
    IMAGE_TAG,
    admin_ws,
    bridge_diag,
    bring_bridges_to_serving,
    claim_admin_api,
    docker_build,
    find_free_ports,
    get_repo_root,
    register_primary_domain,
    remove_container,
    restart_service,
    start_container_with_ports,
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
    pytest.mark.self_contained_docker,
]

DOMAIN = "localhost"
CLAIM_CODE = "CONFDG"
SERVICE = "fauna-mail-bridge-mta"
RUN_SCRIPT = f"/run/service/{SERVICE}/run"
SANDBOX_WRAPPER = "fauna-sandbox bridge -- "
TARGET = "mta:confinement_degraded"
#: The restarted bridge must re-dial, re-attest and flush: a service boot, not
#: a fixed delay (convention 14).
LOGS_WAIT_S = 120.0


@pytest.fixture(scope="module")
def docker_image():
    docker_build(get_repo_root())
    yield IMAGE_TAG


@pytest.fixture()
def mail_serving_nest(docker_image):
    http_port, *mail = find_free_ports(5)
    mail_ports = dict(zip((25, 465, 587, 993), mail))
    name = f"fauna-confdeg-{http_port}"
    start_container_with_ports(name, {3000: http_port, **mail_ports}, env={
        "FAUNA_CLAIM_CODE": CLAIM_CODE,
        "FAUNA_PORT": "3000",
    })
    try:
        wait_for_health(http_port, name)
        admin = claim_admin_api(http_port, CLAIM_CODE, handle="admin")
        nest = {
            "name": name,
            "port": http_port,
            "url": f"https://127.0.0.1:{http_port}",
            "admin": admin,
            "mail_ports": mail_ports,
        }
        register_primary_domain(nest, DOMAIN)
        bring_bridges_to_serving(name, nest, mail_ports, DOMAIN)
        yield nest
    finally:
        remove_container(name)


def _entries(nest) -> list[dict]:
    with admin_ws(nest) as admin:
        return admin.call("fauna.admin.logs", {})["entries"]


def _exec_root(name: str, argv: list[str]) -> str:
    proc = subprocess.run(
        ["docker", "exec", "-u", "root", name, *argv],
        capture_output=True, text=True, timeout=30,
    )
    assert proc.returncode == 0, f"{argv} failed in {name}: {proc.stderr}"
    return proc.stdout


@pytest.mark.feature("nest-hardening")
def test_an_unsandboxed_mail_helper_warns_in_the_admins_log(mail_serving_nest):
    nest = mail_serving_nest
    name = nest["name"]

    # The control: the shipped, sandboxed bridge came up serving and warned of
    # nothing — so a warning after the edit is the edit's.
    before = [e for e in _entries(nest) if e["target"] == TARGET]
    assert not before, (
        f"a sandboxed bridge must not report itself degraded: {before}\n"
        f"── bridge diagnostics ──\n{bridge_diag(name)}"
    )

    # The misconfiguration: the run script without its sandbox wrapper.
    script = _exec_root(name, ["cat", RUN_SCRIPT])
    assert SANDBOX_WRAPPER in script, (
        f"the shipped run script no longer wraps the bridge in {SANDBOX_WRAPPER!r}; "
        f"this test's premise moved:\n{script}"
    )
    _exec_root(name, ["sed", "-i", f"s/{SANDBOX_WRAPPER}//", RUN_SCRIPT])
    assert SANDBOX_WRAPPER not in _exec_root(name, ["cat", RUN_SCRIPT])
    restart_service(name, SERVICE)

    def _warned():
        hits = [e for e in _entries(nest) if e["target"] == TARGET]
        return hits[-1] if hits else None

    entry = wait_until(
        _warned, LOGS_WAIT_S, interval=1.0,
        diagnose=lambda: (
            f"targets on fauna.admin.logs: {sorted({e['target'] for e in _entries(nest)})}"
            f"\n── bridge diagnostics ──\n{bridge_diag(name)}"
        ),
    )
    assert str(entry.get("level", "")).lower().startswith("warn"), (
        f"the degraded probe must reach the admin as a warning: {entry}"
    )
    message = entry["message"]
    assert "landlock" in message and "sealed store" in message, (
        f"the warning must name what is wrong (its sandbox and the sealed store): "
        f"{message!r}"
    )

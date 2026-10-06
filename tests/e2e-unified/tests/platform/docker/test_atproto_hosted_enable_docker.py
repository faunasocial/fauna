"""tier_4 e2e: the ``fauna-atproto-bridge`` s6 service is down-by-default in
the real Docker deployment image, and boots when nest's PRODUCTION enable path
writes ``/data/atproto-enabled`` -- the atproto twin of
``test_mail_client_ui_enable_docker.py``.

The Docker image's s6 run-script (``docker/s6/fauna-atproto-bridge/run``)
gates the bridge on that flag: absent at boot ⇒ ``s6-svc -d`` (self-down).
Only nest's ``set_atproto_enabled`` (``bins/fauna-nest/src/mail_enable.rs``)
writes the flag and best-effort nudges the supervisor sidekick socket to
``s6-svc -u`` the service without a restart. A binary-spawn test bypasses that
run-script entirely, so it can never catch a deployment in which the flag
write or the supervisor notify silently regressed -- exactly the class of bug
tier_4 (real image + s6 supervision) exists to catch (see
``test_mail_client_ui_enable_docker.py``'s module docstring for the mail-side
precedent that motivated this pattern).

``set_atproto_enabled`` is fired unconditionally the first time any user
reaches a **hosted** integration level, from inside
``fauna.bridges.atproto.set_integration_level`` -- the depth selector's one
USER-class mutation kind (``bridge_atproto_handlers.rs``
``set_integration_level_handler``; ``docs/goal/ui/atproto.md`` § Where logic
lives: "The nest half of hosted-enable exists ... composed entirely by the
transition kind -- the first user to reach a hosted level boots the PDS
bridge."). This test drives that exact RPC as the claimed admin (a handled
actor, which is all the USER-class self-scoped check requires) rather than
through a client's UI widget: the app-UI-only equivalent is already proven by
``tests/e2e-unified/tests/test_atproto_settings.py::test_hosted_ladder_through_the_card``,
which drives the same transition kind through the depth-selector card on a
real client driver (e2e convention 8's cited-UI-proof exception). What is
still unproven anywhere else is whether the real deployment *artifact*
actually wires the flag write through to a running s6 service -- the thing
only tier_4 can catch.

``did_method="web"`` with an empty rotation pubkey needs no external PLC
directory (unlike ``test_atproto_identity_mint.py``'s ``did:plc`` flow) --
this test's unit is the s6 bring-up, not the DID mint pipeline, and the flag
write fires from the level transition alone, before any mint completes.

Historical note: ``test_atproto_pds_sni_router.py`` and ``enable_atproto_bridge``
(``helpers.py``) predate this RPC and still say "there is no production admin
enable yet -- S4" -- that was true when written but the transition kind has
since landed (``docs/goal/behavior/atproto-pds-full.md`` § Implementation
status today, F4). Both stay on the infra-level ``touch`` + ``s6-svc -u``
helper regardless: their unit under test is SNI routing / the approve-then-
serve lifecycle, not the enable path itself, so driving the real RPC there
would only add an unrelated dependency.
"""

import subprocess
import time

import pytest

from .helpers import (
    ATPROTO_ENABLE_FLAG_PATH,
    ATPROTO_SERVICE,
    admin_ws,
    atproto_bridge_logs,
    atproto_flag_present,
    claim_admin_api,
    docker_build,
    find_free_port,
    get_repo_root,
    is_commanded_up,
    remove_container,
    start_container,
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
    pytest.mark.self_contained_docker,
]

CLAIM_CODE = "ATPUD1"


@pytest.fixture()
def docker_nest():
    """A fresh **claimed** nest running as the real Docker image. Nothing here
    touches the atproto bridge -- the unit under test is whether the
    PRODUCTION enable path (the transition kind) boots it, so the fixture must
    start from the untouched down-by-default state."""
    docker_build(get_repo_root())

    port = find_free_port()
    name = f"fauna-atproto-uienable-{port}"
    start_container(name, port, env={"FAUNA_CLAIM_CODE": CLAIM_CODE})
    try:
        wait_for_health(port, name)
        admin = claim_admin_api(port, CLAIM_CODE, handle="admin")
        yield {
            "name": name,
            "port": port,
            "url": f"https://127.0.0.1:{port}",
            "admin": admin,
        }
    finally:
        remove_container(name)


@pytest.mark.feature("atproto")
def test_atproto_hosted_enable_boots_docker_bridge(docker_nest):
    name = docker_nest["name"]

    # ── Down by default: no flag, service not commanded up ──────────────────
    assert not atproto_flag_present(name), (
        f"{ATPROTO_ENABLE_FLAG_PATH} must not exist on a fresh, untouched "
        f"deployment -- the atproto bridge is down-by-default."
    )
    initial = svstat(name, ATPROTO_SERVICE)
    assert not is_commanded_up(initial), (
        f"{ATPROTO_SERVICE} must not be commanded up before anyone enables "
        f"atproto; svstat={initial!r}"
    )

    # ── The production enable path: a handled user enters a hosted level ────
    # (fauna.bridges.atproto.set_integration_level -- the depth selector's one
    # mutation kind; see module docstring for the cited UI-driven proof.)
    with admin_ws(docker_nest) as admin:
        admin.call(
            "fauna.bridges.atproto.set_integration_level",
            {
                "target_level": "hosted_visible",
                "did_method": "web",
                "user_rotation_pub_did_key": "",
                "history_backfill": False,
            },
        )

    # ── Entering a hosted level must BOOT the deployed bridge ───────────────
    deadline = time.monotonic() + 30.0
    flag = False
    up = False
    while time.monotonic() < deadline and not (flag and up):
        flag = atproto_flag_present(name)
        up = is_commanded_up(svstat(name, ATPROTO_SERVICE))
        if flag and up:
            break
        time.sleep(0.5)

    assert flag, (
        f"entering a hosted atproto level did not write {ATPROTO_ENABLE_FLAG_PATH} "
        f"-- set_integration_level should fire set_atproto_enabled(true) the "
        f"first time a user reaches a hosted level (bridge_atproto_handlers.rs "
        f"set_integration_level_handler); check that path didn't regress.\n"
        f"bridge log tail:\n" + "\n".join(atproto_bridge_logs(name).splitlines()[-40:])
    )
    assert up, (
        f"{ATPROTO_SERVICE} was not commanded up within {30.0}s of the flag "
        f"write -- either the supervisor sidekick notify regressed, or the "
        f"s6 run-script's flag guard did not react.\n"
        f"svstat={svstat(name, ATPROTO_SERVICE)!r}\n"
        f"bridge log tail:\n" + "\n".join(atproto_bridge_logs(name).splitlines()[-40:])
    )

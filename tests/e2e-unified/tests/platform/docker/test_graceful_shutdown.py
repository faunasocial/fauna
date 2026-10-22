"""tier_4 — graceful shutdown over the real Docker image + SIGTERM.

Proves the deployment-fidelity half of `transport.md` § Graceful shutdown that a
bare-binary (tier_3) test cannot: a real ``docker stop`` (SIGTERM to the
containerized nest) makes the nest broadcast a clean WS 1001 (Going Away) to a
connected client — never 1000 (which would stop the client's reconnect loop on
every Watchtower redeploy) and never a bare TCP drop with no close frame (which
the client would only notice after its dead-link timeout).

The 1001 close code, the in-flight drain, and the never-1000 invariant are
proven at the protocol level by ``bins/fauna-nest/tests/graceful_shutdown.rs``
(tier_3). This test adds the one thing only the real image shows: SIGTERM
actually reaches the nest through the container lifecycle and the 1001 reaches
the wire before the process exits.
"""

import subprocess
import time

import pytest

from drivers.port_util import popen_group_kwargs, reap_descendants_of

from .helpers import (
    authed_ws,
    claim_admin_api,
    docker_build,
    find_free_port,
    get_repo_root,
    read_close_code,
    remove_container,
    start_container,
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


@pytest.fixture(scope="module")
def docker_image():
    """Build (or reuse, via the warm buildx cache) the nest image."""
    yield docker_build(get_repo_root())
    # Leave the image tag for sibling docker modules to reuse — the tag is keyed
    # on this tree's sources, so they find it and build nothing.


@pytest.mark.feature("nest-hardening")
def test_docker_stop_emits_ws_1001(docker_image):
    """`docker stop` (SIGTERM) → the connected client gets a clean WS 1001."""
    port = find_free_port()
    name = f"fauna-nest-shutdown-{port}"
    claim_code = "TE5T01"
    start_container(name, port, env={
        "FAUNA_MODE": "public",
        "FAUNA_CLAIM_CODE": claim_code,
    })
    try:
        wait_for_health(port, name)
        admin = claim_admin_api(port, claim_code)
        ws = authed_ws(port, bytes(admin["signing_key"].verify_key).hex(), admin["token"])
        try:
            # Let the server-side subscription land.
            time.sleep(0.5)
            # SIGTERM via `docker stop`, non-blocking so we can read the close
            # frame concurrently. -t 15 matches the compose stop_grace_period.
            stop = subprocess.Popen(
                ["docker", "stop", "-t", "15", name],
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                **popen_group_kwargs(),
            )
            reap_descendants_of(stop.pid)
            try:
                code = read_close_code(ws, timeout=20.0)
            finally:
                stop.wait(timeout=30)
            assert code == 1001, (
                f"docker stop must close the WS with 1001 (Going Away); got {code!r}. "
                "1000 would stop the client's reconnect loop forever; None means a "
                "bare TCP drop with no close frame (the pre-graceful-shutdown gap)."
            )
        finally:
            try:
                ws.close()
            except Exception:
                pass
    finally:
        remove_container(name)


# How long the wrapper below holds the SIGTERM before the real nest sees it:
# the nest's worst-case stop (the 7 s drain wait plus finalize and checkpoint),
# and well past s6-overlay's 3 s default S6_SERVICES_GRACETIME /
# S6_KILL_GRACETIME, either of which would cut it if it bounded the nest's stop.
_SLOW_STOP_S = 8

# Stands in for the nest's run-script: runs the image's real one as a child and
# forwards s6-supervise's SIGTERM only after _SLOW_STOP_S, so the fauna-nest
# service takes that long to go down while the real nest keeps its WS open.
_SLOW_STOP_RUN = f"""#!/bin/bash
/etc/fauna-test/nest-run &
child=$!
on_term() {{
  sleep {_SLOW_STOP_S}
  kill -TERM "$child"
  wait "$child"
  exit $?
}}
trap on_term TERM
wait "$child"
"""


@pytest.mark.feature("nest-hardening")
def test_slow_nest_stop_fits_s6_grace(docker_image, tmp_path):
    """A nest stop longer than s6-overlay's 3 s defaults still ends in 1001, unkilled.

    `installers/docker.md` § s6-overlay Services records the measurement this
    pins: s6-rc waits for `fauna-nest` without bound (S6_SERVICES_GRACETIME does
    not cut it), and S6_KILL_GRACETIME adds a ~3 s sweep after the last service
    is down, so the nest's stop has about 12 of the 15 s `stop_grace_period`.
    The nest's own drain cannot be made slow in a release image, so the
    service's stop is lengthened from outside: its SIGTERM is held back
    `_SLOW_STOP_S` seconds, then the real nest drains and broadcasts 1001.
    """
    port = find_free_port()
    name = f"fauna-nest-slowstop-{port}"
    claim_code = "TE5T02"
    wrapper = tmp_path / "run"
    wrapper.write_text(_SLOW_STOP_RUN)
    wrapper.chmod(0o755)
    nest_run = get_repo_root() / "docker" / "s6" / "fauna-nest" / "run"
    start_container_with_ports(
        name, {443: port},
        env={
            "FAUNA_PORT": "3000",
            "FAUNA_MODE": "public",
            "FAUNA_CLAIM_CODE": claim_code,
        },
        mounts=[
            (str(wrapper), "/etc/s6-overlay/s6-rc.d/fauna-nest/run"),
            (str(nest_run), "/etc/fauna-test/nest-run"),
        ],
    )
    try:
        wait_for_health(port, name)
        admin = claim_admin_api(port, claim_code)
        ws = authed_ws(port, bytes(admin["signing_key"].verify_key).hex(), admin["token"])
        try:
            time.sleep(0.5)
            began = time.monotonic()
            stop = subprocess.Popen(
                ["docker", "stop", "-t", "15", name],
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                **popen_group_kwargs(),
            )
            reap_descendants_of(stop.pid)
            try:
                code = read_close_code(ws, timeout=20.0)
                closed_after = time.monotonic() - began
            finally:
                stop.wait(timeout=30)
            assert code == 1001, (
                f"a {_SLOW_STOP_S} s nest stop must still close the WS with 1001; "
                f"got {code!r} after {closed_after:.1f} s. None means the nest was "
                "killed before its drain — s6 cut the fauna-nest service short."
            )
            assert closed_after >= _SLOW_STOP_S, (
                f"the 1001 came {closed_after:.1f} s after docker stop, before the "
                f"wrapper's {_SLOW_STOP_S} s hold ended — the wrapper did not "
                "hold the SIGTERM, so this run proves nothing about s6's grace."
            )
        finally:
            try:
                ws.close()
            except Exception:
                pass
        exit_code = subprocess.run(
            ["docker", "inspect", name, "--format", "{{.State.ExitCode}}"],
            capture_output=True, text=True, timeout=10,
        ).stdout.strip()
        assert exit_code == "0", (
            f"the container exited {exit_code} (137 is docker's SIGKILL at the "
            f"15 s stop grace): a {_SLOW_STOP_S} s nest stop plus s6-overlay's "
            "shutdown sweep no longer fits inside `stop_grace_period`."
        )
    finally:
        remove_container(name)

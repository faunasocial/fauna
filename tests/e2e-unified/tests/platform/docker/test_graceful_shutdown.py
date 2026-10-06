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

import ssl
import struct
import subprocess
import time

import pytest
import websocket  # from websocket-client

from .helpers import (
    claim_admin_api,
    docker_build,
    find_free_port,
    get_repo_root,
    remove_container,
    start_container,
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
    # Leave the image tag for sibling docker modules to reuse — they rebuild the
    # same `fauna-nest-test:local` against the buildx cache.


def _authed_ws(port: int, actor_id_hex: str, token: str) -> "websocket.WebSocket":
    """Open the authenticated WS-RPC socket to a docker-published nest.

    Self-signed bootstrap cert → verify off. The *authenticated* endpoint binds
    the actor, so the connection is counted in the nest's registry and the
    shutdown drain waits for it to close + flush its 1001 before the process
    exits (an anonymous connection isn't counted, so its 1001 could race the
    exit — see `ws::GRACEFUL_SHUTDOWN_TIMEOUT`)."""
    url = f"wss://127.0.0.1:{port}/api/v1/ws/{actor_id_hex}"
    return websocket.create_connection(
        url,
        subprotocols=["fauna.v1", f"bearer.{token}"],
        sslopt={"cert_reqs": ssl.CERT_NONE},
        timeout=10,
    )


def _read_close_code(ws: "websocket.WebSocket", timeout: float = 20.0) -> int | None:
    """Read frames until the server's Close frame; return its 2-byte status code
    (None if the socket dropped without one)."""
    ws.settimeout(timeout)
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        try:
            frame = ws.recv_frame()
        except websocket.WebSocketTimeoutException:
            continue
        except (websocket.WebSocketConnectionClosedException, OSError):
            break
        if frame is None:
            continue
        if frame.opcode == websocket.ABNF.OPCODE_CLOSE:
            return struct.unpack("!H", frame.data[:2])[0] if len(frame.data) >= 2 else None
        # ignore binary / ping / pong frames
    return None


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
        ws = _authed_ws(port, bytes(admin["signing_key"].verify_key).hex(), admin["token"])
        try:
            # Let the server-side subscription land.
            time.sleep(0.5)
            # SIGTERM via `docker stop`, non-blocking so we can read the close
            # frame concurrently. -t 15 matches the compose stop_grace_period.
            stop = subprocess.Popen(
                ["docker", "stop", "-t", "15", name],
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
            )
            try:
                code = _read_close_code(ws, timeout=20.0)
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

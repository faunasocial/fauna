"""tier_4: a helper inside the nest cannot fake where a connection comes from —
``security.md`` § Co-resident process trust boundary (UID isolation), the forged
PROXY-header half, on the real image.

Every helper the image runs (the mail bridges, the relay, …) reaches the nest
over loopback, the same position the SNI router dials from. The router conveys a
client's real address in a PROXY-v2 header, and the nest honours that header
only when it carries the router's secret, which no helper can read
(``test_proxy_router_auth.py``). A header without it is answered with the true
loopback peer (``lib.rs::read_optional_proxy_header``), so every per-address
limit and lockout keyed on the source still keys on the helper itself.

**How the forger is placed.** A stock python container joins the nest
container's network namespace and runs ``fakes/proxy_forger.py``: each connection
the host test opens to it is spliced to the nest at ``127.0.0.1:3000`` behind a
PROXY-v2 header claiming ``FORGED_SOURCE``. So the nest sees exactly what a
compromised helper could send, and the test's ordinary WS-RPC client signs in
through it unchanged.

**What is observed.** The nest records the address each sign-in came from
(``auth_core::note_sign_in_address`` → ``actor_last_ip``), the same attribution
the per-address throttles read. The admin signs in once directly from the host
(the control: a non-loopback address is recorded, so the column is live), then
once through the forger; the recorded address must become loopback — the forger's
real address — and never the address it claimed. That the nest *read* the forged
header rather than ignoring PROXY headers altogether is proven by the sign-in
succeeding at all: an unconsumed header would sit in front of the TLS
ClientHello and break the handshake.
"""

from __future__ import annotations

import subprocess

import pytest

from .helpers import (
    IMAGE_TAG,
    SCANNER_SIDECAR_IMAGE,
    claim_admin_api,
    docker_build,
    ensure_image,
    fence_cpuset_args,
    find_free_ports,
    get_repo_root,
    remove_container,
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

CLAIM_CODE = "FORGE9"
#: TEST-NET-3 (RFC 5737): an address nothing real will ever arrive from.
FORGED_SOURCE = "203.0.113.77"
#: The port the forger listens on inside the shared network namespace.
FORGER_PORT = 4999


@pytest.fixture(scope="module")
def docker_image():
    docker_build(get_repo_root())
    yield IMAGE_TAG


@pytest.fixture()
def forged_nest(docker_image):
    http_port, fwd_port = find_free_ports(2)
    name = f"fauna-forge-{http_port}"
    volume = f"{name}-data"
    forger = f"{name}-forger"
    subprocess.run(["docker", "volume", "create", volume],
                   check=True, capture_output=True, timeout=30)
    try:
        start_container_with_ports(
            name, {3000: http_port, FORGER_PORT: fwd_port},
            env={"FAUNA_CLAIM_CODE": CLAIM_CODE, "FAUNA_PORT": "3000"},
            data_volume=volume,
        )
        wait_for_health(http_port, name)
        admin = claim_admin_api(http_port, CLAIM_CODE, handle="admin")

        ensure_image(SCANNER_SIDECAR_IMAGE)
        fakes = str(get_repo_root() / "tests" / "e2e-unified" / "fakes")
        r = subprocess.run(
            ["docker", "run", "-d", *fence_cpuset_args(), "--name", forger,
             "--network", f"container:{name}",
             "-v", f"{fakes}:/fakes:ro", SCANNER_SIDECAR_IMAGE,
             "python", "/fakes/proxy_forger.py",
             "--listen", f"0.0.0.0:{FORGER_PORT}",
             "--upstream", "127.0.0.1:3000",
             "--forge", FORGED_SOURCE],
            capture_output=True, text=True, timeout=60,
        )
        assert r.returncode == 0, f"forger sidecar failed to start: {r.stderr}"
        yield {"name": name, "volume": volume, "forger": forger,
               "http_port": http_port, "fwd_port": fwd_port, "admin": admin}
    finally:
        remove_container(forger)
        remove_container(name)
        subprocess.run(["docker", "volume", "rm", "-f", volume],
                       capture_output=True, timeout=30)


def _sign_in(url: str, admin: dict) -> None:
    """One authenticated WS-RPC session: its handshake is the sign-in the nest
    attributes to an address."""
    from clients.ws_rpc_admin_client import WsRpcAdminClient

    sk = admin["signing_key"]
    with WsRpcAdminClient(url, actor_id=bytes(sk.verify_key), signing_key=bytes(sk)) as c:
        c.call("fauna.nest.info", {})


def _recorded_address(nest: dict) -> str | None:
    """The address the nest last attributed the admin's sign-in to, read from
    its own database through a sidecar sharing the data volume."""
    actor = bytes(nest["admin"]["signing_key"].verify_key).hex()
    snippet = (
        "import sqlite3; c = sqlite3.connect('/data/nest.db', timeout=30); "
        "r = c.execute('SELECT ip_address FROM actor_last_ip WHERE actor_id = ?', "
        f"(bytes.fromhex('{actor}'),)).fetchone(); print(r[0] if r else '')"
    )
    r = subprocess.run(
        ["docker", "run", "--rm", "-v", f"{nest['volume']}:/data",
         SCANNER_SIDECAR_IMAGE, "python", "-c", snippet],
        capture_output=True, text=True, timeout=60,
    )
    assert r.returncode == 0, f"reading actor_last_ip failed: {r.stderr}"
    return r.stdout.strip() or None


@pytest.mark.feature("nest-hardening")
def test_a_helper_cannot_fake_where_a_connection_comes_from(forged_nest):
    nest = forged_nest

    # The control: a sign-in straight from the host is attributed to the host's
    # real, non-loopback address — the column is live and keyed per sign-in.
    _sign_in(f"https://127.0.0.1:{nest['http_port']}", nest["admin"])
    direct = _recorded_address(nest)
    assert direct and not direct.startswith("127."), (
        f"a direct sign-in must be attributed to its non-loopback source: {direct!r}"
    )

    # The forgery: the same sign-in through a co-resident helper that claims
    # FORGED_SOURCE in a PROXY-v2 header without the router's secret.
    _sign_in(f"https://127.0.0.1:{nest['fwd_port']}", nest["admin"])
    forged = _recorded_address(nest)
    assert forged != FORGED_SOURCE, (
        f"the nest believed a helper's forged PROXY header: the sign-in was "
        f"attributed to {FORGED_SOURCE}, so a helper can dodge every per-address "
        f"limit and lockout by naming a fresh address each time"
    )
    assert forged == "127.0.0.1", (
        f"a forged header must be answered with the helper's true loopback "
        f"address; recorded {forged!r} (direct sign-in was {direct!r})"
    )

"""E2E test: claiming a Docker nest over the real wire — no app, no browser.

Split out of ``test_docker_e2e.py`` (2026-08-30) so its surface classification
is true at the module level: this test drives **no app driver** — every leg
goes over the deployed image's anonymous WS-RPC surface — which makes it a
``[nest]`` witness (`docs/goal/architecture/feature-catalog.md` § The two
surfaces: proven once, app-independently, observed from outside; counts for
every column). Inside ``test_docker_e2e.py`` the module-wide
``browser_session`` import made the catalog's driver-closure scan read it as
app-driving, so the outcome it witnesses was pinned to the web column alone —
the wide parity gap this split closes. The browser variant stays behind: the
two prove complementary tier_4-unique packaging facts, and only that one
needs a browser.

The container boots **domainless** with a known claim code injected via
``FAUNA_CLAIM_CODE``, exactly as the sibling module's.
"""

import subprocess
import time

import pytest

from .helpers import (
    docker_build,
    find_free_port,
    generate_claim_code,
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

# No app marker on purpose: the test launches no app driver, so it is
# client-independent — selected like the `tests/api/` suites, not by `--client`.
pytestmark = [
    pytest.mark.skipif(not HAS_DOCKER, reason="Docker not available"),
    pytest.mark.tier_4,
]


@pytest.fixture(scope="module")
def docker_image():
    """Build the Docker image once per module."""
    repo = get_repo_root()
    tag = docker_build(repo)
    yield tag


@pytest.fixture()
def nest(docker_image):
    """Start a fresh container with a known claim code.

    Yields dict with port, claim_code, container_name, nest_url.
    Cleans up the container after the test.
    """
    port = find_free_port()
    claim_code = generate_claim_code()
    name = f"fauna-nest-e2e-{port}"
    start_container(name, port, env={
        "FAUNA_MODE": "public",
        "FAUNA_CLAIM_CODE": claim_code,
    })
    try:
        wait_for_health(port, name)
        yield {
            "port": port,
            "claim_code": claim_code,
            "container_name": name,
            "nest_url": f"https://127.0.0.1:{port}",
        }
    finally:
        remove_container(name)


@pytest.mark.feature("claim-a-fresh-nest")
def test_docker_api_provisioning(nest):
    """Claiming admin works over the real wire in the image, and setup-status
    does NOT leak the one-time claim code.

    All legs go over the deployed image's anonymous WS-RPC surface
    (``fauna.setup.status``, ``fauna.auth.claim_admin``) — the claim-code-leak
    check is the security-critical assert (a setup-status that echoed the code
    would hand any anonymous caller admin). The richer mail-enable provisioning
    is covered by ``test_mail_client_ui_enable_docker.py``.
    """
    from nacl.signing import SigningKey

    from clients.ws_rpc_anon_client import WsRpcAnonClient

    url = nest["nest_url"]
    claim_code = nest["claim_code"]

    # ── Phase 1: setup-status reports unclaimed and does NOT leak the code ──
    with WsRpcAnonClient(url) as anon:
        status = anon.call("fauna.setup.status", {})
    assert status["claimed"] is False
    assert "claim_code" not in status, "setup-status must not expose claim_code"

    # ── Phase 2: Claim admin via the anonymous WS-RPC kind ──
    sk = SigningKey.generate()
    actor_id_hex = bytes(sk.verify_key).hex()
    timestamp = int(time.time())
    from common.sig_domain import claim_admin_signed_message

    msg = claim_admin_signed_message(bytes(sk.verify_key), timestamp)
    sig = sk.sign(msg).signature

    with WsRpcAnonClient(url) as anon:
        claim_reply = anon.call(
            "fauna.auth.claim_admin",
            {
                "claim_code": claim_code,
                "actor_id": actor_id_hex,
                "signature": sig.hex(),
                "timestamp": timestamp,
                "handle": "apiadmin",
            },
        )
    assert claim_reply["token"], "claim_admin must return a bearer token"

    # ── Phase 3: setup-status now reports claimed ──
    with WsRpcAnonClient(url) as anon:
        status = anon.call("fauna.setup.status", {})
    assert status["claimed"] is True

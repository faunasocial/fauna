"""tier_4 e2e: the fauna-sni-router fronts container :443 and L4-routes
``pds.<domain>`` to the ATProto PDS bridge, which terminates its own TLS and
keys its createSession rate-limit on the REAL client IP conveyed via PROXY-v2.

This is the F1-packaging acceptance (``docs/goal/behavior/atproto-pds-full.md``
§ Implementation status → "Not yet landed (F1 remainder) (b)"). The serving path
is already unit- + tier_3-verified; only tier_4 exercises the pieces the binary
tests bypass: the **s6 router run-script** actually wiring ``--route
"pds.*=127.0.0.1:8447"`` + ``--send-proxy-to``, the **s6 bridge run-script** on
``--xrpc-listen=127.0.0.1:8447``, and the whole enrol→approve→x25519-attest→
seal-read-cert→serve lifecycle inside the deploy image.

What this asserts, against host :443 (the router):

  A. (``test_sni_router_routes_pds_to_the_atproto_bridge``) SNI ``pds.<domain>``
     GET ``com.atproto.server.describeServer`` returns 200 with the PDS service
     DID ``did:web:pds.<domain>`` — proof the encrypted byte stream was routed by
     SNI to the bridge on 8447, which terminates its own ``pds.<domain>`` TLS
     (nest has no ``/xrpc`` surface). A bad-credential createSession is a clean
     401 from the bridge (routed + auth plane reached), not a route failure.

  B. (``test_createsession_rate_limit_carries_real_client_ip``) Driving >10
     createSession/5min over the router trips the per-IP ClassAuth rate limit
     (``internal/xrpc/ratelimit.go``: 10/5min, checked before the handler), and
     the bridge logs ``xrpc: source-IP rate limit exceeded`` carrying the real
     ``source_ip`` — the docker **gateway** IP (non-loopback) the router conveys
     via PROXY-v2, NOT the router's loopback. A loopback ``source_ip`` would mean
     the router's ``--send-proxy-to "127.0.0.1:8447"`` header never reached the
     bridge's ``proxyproto`` peel and the rate limit keyed on the router, not the
     client. This is the exposed-port real-client-IP half the b.1 unit test
     (``TestXrpcListenerStackKeysRateLimitOnProxyV2ClientIP``) can't reach — it
     bypasses the Dockerfile + s6 router run-script that actually wire it.

The host→published-port path arrives at the in-container router as the docker
bridge **gateway** IP (172.17.0.1 on the default bridge with userland-proxy) — a
guaranteed non-loopback source — so the router conveys a genuinely remote
address, exactly as the sibling ``test_caldav_sni_router.py`` § D proves for the
MDA's CalDAV lockout.

The atproto.pds bridge is down-by-default and NEVER auto-approves, so the fixture
enables it at the infra level and manually approves it (there is no production
admin enable yet — S4); see ``bring_atproto_bridge_to_serving``.
"""

import ipaddress
import json
import re
import ssl
import subprocess
import time

import pytest

from .helpers import (
    IMAGE_TAG,
    bring_atproto_bridge_to_serving,
    claim_admin_api,
    docker_build,
    find_free_ports,
    get_repo_root,
    register_primary_domain,
    remove_container,
    sni_https_request,
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
CLAIM_CODE = "PDSSNI"


# ── Fixtures ──────────────────────────────────────────────────────────


@pytest.fixture(scope="module")
def docker_image():
    """Build the image once for this module (tag shared; rebuild is layer-cached)."""
    docker_build(get_repo_root())
    yield IMAGE_TAG


@pytest.fixture(scope="module")
def serving_atproto(docker_image):
    """A container with nest + the atproto PDS bridge brought to serving, the
    router's :443 mapped to a host port, admin claimed, primary domain registered.
    Yields the mapped host ports + admin creds. Cleaned up after.

    Module-scoped so the two acceptance tests share one (expensive) bring-up; they
    don't interfere — test A never floods, and test B keys the per-IP ClassAuth
    window on the single docker-gateway IP both share (idempotent to re-drive)."""
    http_port, router_port = find_free_ports(2)
    name = f"fauna-atproto-router-{http_port}"
    start_container_with_ports(
        name,
        {3000: http_port, 443: router_port},
        env={"FAUNA_CLAIM_CODE": CLAIM_CODE, "FAUNA_PORT": "3000"},
    )
    try:
        wait_for_health(http_port, name)
        admin = claim_admin_api(http_port, CLAIM_CODE, handle="admin")
        nest = {
            "name": name,
            "port": http_port,
            "router_port": router_port,
            "url": f"https://127.0.0.1:{http_port}",
            "admin": admin,
        }
        register_primary_domain(nest, DOMAIN)
        bring_atproto_bridge_to_serving(name, nest, DOMAIN)

        # Readiness: the TLS-terminating listener can log "up" a beat before its
        # cert refresh completes (soft first Refresh + backoff). Retry the public
        # describeServer over SNI until the handshake + route succeed, so both
        # tests can assume a serving endpoint. describeServer is ClassPublicRead
        # (60/60s) — a different bucket from createSession's ClassAuth, so these
        # probes never consume the rate-limit budget test B drives.
        deadline = time.monotonic() + 60
        last = None
        while time.monotonic() < deadline:
            try:
                status, _h, _b = sni_https_request(
                    router_port, f"pds.{DOMAIN}", "GET",
                    "/xrpc/com.atproto.server.describeServer", timeout=8)
                if status == 200:
                    break
                last = f"status={status}"
            except (OSError, ssl.SSLError) as e:
                last = repr(e)
            time.sleep(1.0)
        else:
            raise AssertionError(
                f"atproto XRPC never served 200 over SNI pds.{DOMAIN}: {last}")
        yield nest
    finally:
        remove_container(name)


# ── Helpers ───────────────────────────────────────────────────────────


def _create_session(router_port: int, identifier: str, password: str):
    """POST ``com.atproto.server.createSession`` via SNI ``pds.<domain>`` through
    the router. Returns ``(status, parsed-json-or-{})``."""
    body = json.dumps({"identifier": identifier, "password": password}).encode()
    status, _headers, raw = sni_https_request(
        router_port, f"pds.{DOMAIN}", "POST",
        "/xrpc/com.atproto.server.createSession",
        extra_headers={"Content-Type": "application/json"}, body=body)
    try:
        return status, json.loads(raw or b"{}")
    except json.JSONDecodeError:
        return status, {"_raw": raw.decode(errors="replace")}


# ── Tests ─────────────────────────────────────────────────────────────


@pytest.mark.feature("atproto")
def test_sni_router_routes_pds_to_the_atproto_bridge(serving_atproto):
    """(A) SNI ``pds.<domain>`` on the router :443 reaches the atproto bridge's
    XRPC surface — proof the split routes by SNI to the bridge, not nest."""
    rp = serving_atproto["router_port"]

    status, _headers, body = sni_https_request(
        rp, f"pds.{DOMAIN}", "GET", "/xrpc/com.atproto.server.describeServer")
    assert status == 200, (
        f"SNI pds.{DOMAIN} GET describeServer must reach the atproto bridge; "
        f"got {status}, body={body[:200]!r}")
    parsed = json.loads(body or b"{}")
    assert parsed.get("availableUserDomains") == [], parsed
    # The service DID is derived from the registered primary domain, proving the
    # bridge (not nest) answered and read PrimaryDomain from its config snapshot.
    assert str(parsed.get("did", "")) == f"did:web:pds.{DOMAIN}", parsed

    # (A, negative) A bad-credential createSession is a clean 401 from the bridge's
    # auth plane — routed + reached, not a connection/route failure.
    status, err = _create_session(rp, "probe", "wrong-xxxx-yyyy-zzzz")
    assert status == 401 and err.get("error") == "AuthenticationRequired", (status, err)


@pytest.mark.feature("atproto")
def test_createsession_rate_limit_carries_real_client_ip(serving_atproto):
    """(B) The per-IP createSession rate limit keys on the real client IP the
    router conveys via PROXY-v2, not the router's loopback. See module § B."""
    rp = serving_atproto["router_port"]
    name = serving_atproto["name"]

    # The per-IP ClassAuth window is 10/5min (internal/xrpc/ratelimit.go); the 11th
    # createSession from one IP is refused with 429 *before* the handler runs.
    # Drive comfortably past it within the window so the rejection (and its log)
    # fire. Every request here shares the one docker-gateway source IP.
    saw_429 = False
    for _ in range(15):
        status, _ = _create_session(rp, "probe", "wrong-xxxx-yyyy-zzzz")
        if status == 429:
            saw_429 = True
        else:
            assert status == 401, f"pre-limit createSession must 401; got {status}"
    assert saw_429, (
        "driving >10 createSession/min over the router must trip the per-IP 429 "
        "(ClassAuth 10/5min); none seen — the rate limit did not fire.")

    logs = subprocess.run(
        ["docker", "logs", name], capture_output=True, text=True, timeout=15)
    blob = logs.stdout + logs.stderr
    limit_lines = [ln for ln in blob.splitlines()
                   if "source-IP rate limit exceeded" in ln]
    assert limit_lines, (
        "the per-IP rate-limit rejection must log `xrpc: source-IP rate limit "
        "exceeded`; none seen. Tail:\n" + "\n".join(blob.splitlines()[-30:]))

    # The log surfaces the same client IP the limiter keyed on. A non-loopback
    # value proves the router's PROXY-v2 header reached the bridge's proxyproto
    # peel; a loopback value would mean --send-proxy-to :8447 was unwired.
    ip_re = re.compile(r'source_ip["\s:=]+\s*"?(\d{1,3}(?:\.\d{1,3}){3})')
    seen_ips = {m.group(1) for ln in limit_lines if (m := ip_re.search(ln))}
    assert seen_ips, f"rate-limit lines carried no source_ip: {limit_lines[-3:]!r}"
    assert all(not ipaddress.ip_address(ip).is_loopback for ip in seen_ips), (
        "createSession rate-limit must key on the real client IP conveyed via "
        f"PROXY-v2, not the router's loopback; got source_ip(s)={sorted(seen_ips)}")

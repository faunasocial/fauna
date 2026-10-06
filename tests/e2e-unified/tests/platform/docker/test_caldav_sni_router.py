"""tier_4 e2e: the fauna-sni-router fronts container :443 and L4-routes by SNI.

The SNI-router design (goal doc
``docs/goal/behavior/caldav-server.md`` § Network exposure): on a single-IP box
nest (``<domain>``) and the MDA's CalDAV listener (``mail.<domain>``) both want
:443. A *terminating* proxy would have to decrypt to path-route, becoming an
omniscient plaintext point. Instead ``fauna-sni-router`` peeks the TLS
ClientHello SNI and splices the **encrypted** stream to a backend — nest and the
MDA each terminate their own TLS, so no single process sees the union of their
plaintext.

Only tier_4 can prove this: the router is an **s6 service inside the deploy
image**, the MDA's CalDAV listener is remapped off :443 to loopback 8444 via the
**entrypoint-written operator-hatch** (``caldav_listen_https``), and the
host-port mapping is ``443:443`` (router) not ``443:3000`` (nest). Spawning
binaries directly (tier_3) bypasses all three.

What this asserts, against host :443 (the router):
  A. SNI ``<domain>`` → nest: ``GET /api/v1/health`` returns 200. (Default
     route; the MDA CalDAV has no such path.)
  B. SNI ``mail.<domain>`` → the MDA CalDAV listener: an unauthenticated
     ``PROPFIND /caldav/`` returns ``401`` with ``WWW-Authenticate: Basic
     realm="fauna-caldav"`` (the MDA's realm; ``caldav/auth.go``). The SAME path
     ``GET /api/v1/health`` over this SNI does **not** return nest's 200 — proof
     the byte stream was routed by SNI to a *different* backend, not nest.

The MDA's CalDAV listener only binds once mail is enabled + the bridge is
approved + serving, so this test first drives the full bring-up via
``bring_bridges_to_serving`` (the same path the mail round-trip tests use), then
exercises the router.

This module is also the tier_4 **acceptance** for the exposed-port real-client-IP
propagation chain (tracked internally). Two further tests
assert that the real client IP — not the router's loopback — flows end-to-end in
the deploy image, the half the b.1 unit/integration tests can't reach (they
bypass the Dockerfile + s6 router run-script that actually wires ``--send-proxy-to``):

  C. (``test_external_request_enrollment_refused_over_router``) An external,
     non-loopback client reaching **nest** via the published :443 → router →
     PROXY-v2 path is REFUSED ``fauna.bridges.request_enrollment`` by the (1c)
     loopback gate (``routes.rs``), while the in-container bridge — which dials
     nest's loopback *headerless* — DID enroll (the fixture's
     ``bring_bridges_to_serving`` reaches serving only via that loopback
     enrollment, and ``approved_docker_bridges`` confirms the approved rows). A
     ``127.0.0.1`` here (PROXY-v2 to nest broken / not sent) would PASS the gate
     → a different error → the test fails, so it is a true Task-A regression guard.
  D. (``test_caldav_auth_event_carries_real_client_ip``) Driving failed CalDAV
     BASIC-auth attempts over the router past the default per-IP lockout
     threshold makes the **MDA** log ``caldav: AUTH locked out`` carrying the
     real ``source_ip`` (the docker gateway, non-loopback) — proof the router's
     ``--send-proxy-to "127.0.0.1:8444"`` PROXY-v2 header reaches the MDA's
     ``internal/proxyproto`` listener and the real IP feeds the per-IP lockout +
     ``report_auth_event`` audit (b.1). A loopback ``source_ip`` (PROXY-v2 to the
     MDA broken) would mean the lockout/audit keyed on the router, not the client.

Both reach nest/MDA through the host→published-port path, which docker presents
to the in-container router as the bridge **gateway** IP (verified ``172.17.0.1``
on the default bridge with userland-proxy) — a guaranteed non-loopback source, so
the router conveys a genuinely remote address rather than loopback.
"""

import base64
import ipaddress
import re
import subprocess

import pytest


from .helpers import (
    ROLES,
    IMAGE_TAG,
    admin_ws,
    approved_docker_bridges,
    bring_bridges_to_serving,
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
CLAIM_CODE = "CALDV5"


# ── Fixtures ──────────────────────────────────────────────────────────


@pytest.fixture(scope="module")
def docker_image():
    """Build the image once for this module (tag shared; rebuild is layer-cached)."""
    docker_build(get_repo_root())
    yield IMAGE_TAG


@pytest.fixture(scope="module")
def serving_nest(docker_image):
    """A container with nest + all four mail listeners + the router's :443 mapped
    to host ports, admin claimed, mail brought all the way to serving. Yields the
    mapped host ports + admin creds. Cleaned up after.

    Module-scoped so the three router acceptance tests share one (expensive)
    bring-up. They don't interfere: the SNI-split + enrollment-refusal tests
    never authenticate against CalDAV, and the real-IP lockout test keys its
    per-IP lockout on a throwaway username (`probe@…`) none of the others use."""
    http_port, router_port, *mail = find_free_ports(6)
    mail_ports = dict(zip((25, 465, 587, 993), mail))  # container_port -> host_port
    name = f"fauna-caldav-router-{http_port}"
    start_container_with_ports(
        name,
        {3000: http_port, 443: router_port, **mail_ports},
        env={"FAUNA_CLAIM_CODE": CLAIM_CODE, "FAUNA_PORT": "3000"},
    )
    try:
        wait_for_health(http_port, name)
        admin = claim_admin_api(http_port, CLAIM_CODE, handle="admin")
        # CalDAV + cert provisioning need a committed storage mode; plaintext is
        # the "I trust the box" deploy default.
        nest = {
            "name": name,
            "port": http_port,
            "router_port": router_port,
            "url": f"https://127.0.0.1:{http_port}",
            "admin": admin,
            "mail_ports": mail_ports,
        }
        register_primary_domain(nest, DOMAIN)
        bring_bridges_to_serving(name, nest, mail_ports, DOMAIN)
        yield nest
    finally:
        remove_container(name)


# ── Test ──────────────────────────────────────────────────────────────


@pytest.mark.feature("calendar-in-standard-apps")
def test_sni_router_splits_nest_and_mda_caldav(serving_nest):
    rp = serving_nest["router_port"]

    # (A) SNI <domain> → nest. The router's default backend (127.0.0.1:3000).
    status, _headers, body = sni_https_request(rp, DOMAIN, "GET", "/api/v1/health")
    assert status == 200, (
        f"SNI {DOMAIN!r} on the router :443 must reach nest health; got {status}, body={body[:200]!r}"
    )

    # (B) SNI mail.<domain> → the MDA CalDAV listener (127.0.0.1:8444).
    # The unauthenticated PROPFIND must be challenged with the MDA's own realm —
    # nest never emits `Basic realm="fauna-caldav"`, so this proves the byte
    # stream reached the CalDAV listener, not nest.
    status, headers, body = sni_https_request(
        rp, f"mail.{DOMAIN}", "PROPFIND", "/caldav/")
    assert status == 401, (
        f"SNI mail.{DOMAIN} PROPFIND /caldav/ must be 401 (CalDAV auth challenge); "
        f"got {status}, body={body[:200]!r}"
    )
    www_auth = headers.get("www-authenticate", "")
    assert "fauna-caldav" in www_auth, (
        f"401 must carry the MDA's WWW-Authenticate realm (proves the MDA CalDAV "
        f"listener answered, not nest); got header {www_auth!r}"
    )

    # (B, negative) The SAME path that returned nest's 200 over SNI <domain> must
    # NOT return 200 over SNI mail.<domain> — confirms the split is by SNI, not a
    # single backend answering both.
    status, _headers, _body = sni_https_request(
        rp, f"mail.{DOMAIN}", "GET", "/api/v1/health")
    assert status != 200, (
        f"SNI mail.{DOMAIN} must route to the MDA, which has no /api/v1/health; "
        f"a 200 means it reached nest (router did not split by SNI). got {status}"
    )


# ── Real-client-IP propagation acceptance (Tasks A + B(b.1); see module § C/D) ──


@pytest.mark.feature("nest-hardening")
def test_external_request_enrollment_refused_over_router(serving_nest):
    """(C) An external, non-loopback client is REFUSED ``request_enrollment`` over
    the router, while the in-container bridge enrolled over loopback. See module
    docstring § C."""
    # Positive half — the in-container bridges self-enrolled over nest's loopback
    # (headerless dial → the (1c) loopback gate passes) and were approved: that is
    # the ONLY way the fixture reaches serving. Assert the approved rows exist so
    # the loopback-passes-vs-remote-refused contrast is self-documenting.
    with admin_ws(serving_nest) as admin:
        approved = approved_docker_bridges(admin)
    assert set(ROLES) <= set(approved), (
        "the in-container bridges must have self-enrolled + been approved over "
        f"loopback request_enrollment; approved roles={list(approved)}"
    )

    # Negative half — the SAME kind from a non-loopback peer is refused at the
    # (1c) loopback gate before the payload is even decoded. The host→published
    # port arrives at the in-container router as the docker gateway IP (verified
    # 172.17.0.1, non-loopback); the router conveys it to nest as a PROXY-v2
    # header, which serve_tls resolves into the dispatch peer_addr.
    from clients.ws_rpc_anon_client import RpcCallError, WsRpcAnonClient

    rp = serving_nest["router_port"]
    # An IP-literal base → no SNI on the wire → the router's no-SNI default route
    # = nest (main.rs § backend_for). 127.0.0.1 (not `localhost`) pins the connect
    # to the IPv4-published router port, avoiding a ::1 first-resolution miss.
    with WsRpcAnonClient(f"https://127.0.0.1:{rp}") as anon:
        with pytest.raises(RpcCallError) as ei:
            anon.call("fauna.bridges.request_enrollment", {})
    assert ei.value.code == "fauna.bridges.remote_enrollment_unsupported", (
        "external request_enrollment over the router must be refused by the "
        "loopback gate (real client IP propagated via PROXY-v2, not loopback); "
        f"got code={ei.value.code!r}. A pass / different code means nest saw "
        "127.0.0.1 — PROXY-v2 to nest is broken or the router did not send it."
    )


@pytest.mark.feature("calendar-in-standard-apps")
def test_caldav_auth_event_carries_real_client_ip(serving_nest):
    """(D) The MDA's CalDAV per-IP lockout + ``report_auth_event`` audit key on the
    real client IP the router conveys via PROXY-v2, not the router's loopback. See
    module docstring § D."""
    rp = serving_nest["router_port"]
    name = serving_nest["name"]

    # A well-formed but unresolvable username: ``SplitEmail`` succeeds (so the auth
    # middleware reaches ``resolve`` → ``RecordFailure``), ``validate_recipient``
    # fails (no such user) — every attempt records a per-(user, source-IP) failure
    # without needing a provisioned mailbox. The default lockout threshold is
    # 30/min (``AuthPolicy::default().max_auth_failures_per_minute``); drive
    # comfortably past it within the minute window so the locked branch fires.
    creds = base64.b64encode(b"probe@localhost:wrong-password").decode()
    auth_header = {"Authorization": f"Basic {creds}"}
    for _ in range(40):
        status, _h, _b = sni_https_request(
            rp, f"mail.{DOMAIN}", "PROPFIND", "/caldav/", extra_headers=auth_header)
        assert status == 401, f"bad CalDAV credentials must 401; got {status}"

    logs = subprocess.run(
        ["docker", "logs", name], capture_output=True, text=True, timeout=15,
    )
    blob = logs.stdout + logs.stderr
    locked_lines = [ln for ln in blob.splitlines() if "AUTH locked out" in ln]
    assert locked_lines, (
        "driving >30 failed CalDAV auths/min over the router must trip the MDA's "
        "per-IP lockout (`caldav: AUTH locked out`); none seen. Tail:\n"
        + "\n".join(blob.splitlines()[-30:])
    )

    # The lockout log surfaces the SAME ``sourceIP`` value (`caldav/auth.go:108`,
    # ``clientIP(r)``) that ``report_auth_event`` stamps on its audit row — both
    # read the one variable — so a non-loopback lockout source_ip proves the audit
    # row's source_ip is likewise the real client IP. A loopback value would mean
    # the MDA never peeled PROXY-v2 (router ``--send-proxy-to :8444`` unwired or
    # ``internal/proxyproto`` not in the CalDAV listener chain) and the
    # lockout/audit keyed on the router rather than the attacker.
    ip_re = re.compile(r'source_ip["\s:=]+\s*"?(\d{1,3}(?:\.\d{1,3}){3})')
    seen_ips = {m.group(1) for ln in locked_lines if (m := ip_re.search(ln))}
    assert seen_ips, f"lockout lines carried no source_ip: {locked_lines[-3:]!r}"
    assert all(not ipaddress.ip_address(ip).is_loopback for ip in seen_ips), (
        "CalDAV lockout/audit must key on the real client IP conveyed via "
        f"PROXY-v2, not the router's loopback; got source_ip(s)={sorted(seen_ips)}"
    )

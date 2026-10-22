"""tier_4: A real sidecar event reaches
`admin-logs` through the deployed image.

The sidecar log plane (`observability.md` § The sidecar log plane) lets an
out-of-process bridge report a small, catalogued set of events to nest's
admin Logs surface over its own authenticated WS
(`internal/logplane.Run`/`Flush` → `fauna.bridges.report_log_events` →
`bins/fauna-nest/src/log_plane.rs::admit`, which stamps the wire-visible
`target` as `"<source>:<event>"` — never parsed from the payload, always
derived from the authenticated identity — then merges into the nest's own
ring via `fauna_log::snapshot_merged()`, which `fauna.admin.logs` serves).
Every prior test of this machinery is tier_1 (the catalogue/admission-charset
sweep) or drives the admission function directly; none has ever driven a
REAL Go bridge process, over its REAL WS connection, into a REAL deployed
image's nest and read the result back off `fauna.admin.logs`. That is the
gap this test closes — zero client change, since the existing Settings Logs
page already renders whatever `fauna.admin.logs` returns.

The driving event is `mta`/`mda`'s `"ready"` (`internal/logplane/
catalogue.go::Ready`), emitted right after each role's serving goroutine
starts (`cmd/fauna-mail-bridge/main.go:984`) — the earliest deliverable event
in the catalogue (`logplane.Run` is already ticking by then, so the emit's
non-blocking `nudge` flushes it within `FlushInterval` (2s) rather than
waiting on the ticker). Bringing the bridges all the way to *serving* (via
`bring_bridges_to_serving`, the same sequence `test_mail_deploy_lifecycle.py`
proves step-by-step) is a stronger precondition than `Ready` strictly needs,
but it is the proven, reused path (priority #2) rather than a hand-rolled
shortcut — and it also means the flush loop has had ample time to run by the
time this test reads the ring.

The atproto PDS bridge reports over the same kind with its own catalogue
(`internal/logplane/catalogue_atproto.go` on top of the shared
`catalogue.go`), so the second test brings THAT bridge to serving
(`bring_atproto_bridge_to_serving`, the path every atproto tier_4 test reuses)
and reads its `atproto:ready` back the same way — the nest stamps the
`atproto` source from the bridge's `atproto.pds` class, and the message names
the atproto bridge, never the mail one.

The sni-router reports over the internal sidecar channel instead
(`/internal/router/ws`, `fauna.sidecar.log_events`, the catalogue in
`bins/fauna-sni-router/src/log_catalogue.rs`): its `router:serving` needs
nothing but a booted, claimed image, and its `router:per_ip_cap_shed` is driven
by holding more connections on the published :443 than one source may.
"""

from __future__ import annotations

import subprocess
import time

import pytest

from helpers.waiting import wait_until

from .helpers import (
    IMAGE_TAG,
    ROLES,
    admin_ws,
    atproto_bridge_logs,
    bridge_diag,
    bring_atproto_bridge_to_serving,
    bring_bridges_to_serving,
    claim_admin_api,
    docker_build,
    find_free_ports,
    get_repo_root,
    register_primary_domain,
    remove_container,
    scanner_wired_network,
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
CLAIM_CODE = "LOGPLN"
# Load-robust: the flush loop nudges within FlushInterval (2s) once Emit is
# called, but bring_bridges_to_serving's own setup (keypairs, enrollment,
# approval, restart, cert fan-out) is what actually takes the bulk of the
# time before Ready() even fires — size generously above that, not the flush
# itself (testing.md § point 14).
LOGS_WAIT_S = 60.0


@pytest.fixture(scope="module")
def docker_image():
    """Resolve this tree's keyed image once for this module (``docker_build``
    reuses it when present). Leaves the shared tag in place on teardown, like
    the sibling mail-deploy fixtures."""
    docker_build(get_repo_root())
    yield IMAGE_TAG


@pytest.fixture()
def mail_serving_nest(docker_image):
    """A claimed nest with both mail bridges brought to *serving* inside the
    real Docker image — bring_bridges_to_serving's own proven sequence, so
    Ready() has fired for both roles well before this yields."""
    http_port, *mail = find_free_ports(5)
    mail_ports = dict(zip((25, 465, 587, 993), mail))
    name = f"fauna-logplane-{http_port}"
    with scanner_wired_network("fauna-logplane") as (network, scan_env):
        start_container_with_ports(name, {443: http_port, **mail_ports}, env={
            "FAUNA_CLAIM_CODE": CLAIM_CODE,
            "FAUNA_PORT": "3000",
            **scan_env,
        }, network=network)
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


@pytest.fixture()
def atproto_serving_nest(docker_image):
    """A claimed nest with the atproto PDS bridge brought to *serving* inside the
    real Docker image — `bring_atproto_bridge_to_serving` returns once the
    approved bridge's fresh boot logs "xrpc listener up", right before which
    Ready() fires."""
    (http_port,) = find_free_ports(1)
    name = f"fauna-logplane-atproto-{http_port}"
    start_container_with_ports(name, {443: http_port}, env={
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
        }
        register_primary_domain(nest, DOMAIN)
        bring_atproto_bridge_to_serving(name, nest, DOMAIN)
        yield nest
    finally:
        remove_container(name)


def _log_entries(nest) -> list[dict]:
    with admin_ws(nest) as admin:
        return admin.call("fauna.admin.logs", {})["entries"]


def _await_targets(nest, wanted: set[str]) -> dict[str, dict]:
    """Poll `fauna.admin.logs` until every target in `wanted` is present (or the
    deadline passes); returns the matching entries by target."""
    deadline = time.monotonic() + LOGS_WAIT_S
    by_target: dict[str, dict] = {}
    while time.monotonic() < deadline:
        entries = _log_entries(nest)
        by_target = {e["target"]: e for e in entries if e["target"] in wanted}
        if wanted <= by_target.keys():
            break
        time.sleep(1.0)  # sleep-ok: poll interval of a deadline loop, not a settle-wait
    return by_target


@pytest.mark.feature("admin-logs")
def test_bridge_ready_event_reaches_admin_logs(mail_serving_nest):
    """Both mail-bridge roles' `ready` events cross the sidecar log plane and
    land on `fauna.admin.logs` with the `<source>:<event>` target the nest
    stamps from the bridge's authenticated identity — never from the payload."""
    nest = mail_serving_nest
    wanted = {f"{role}:ready" for role in ROLES}
    by_target = _await_targets(nest, wanted)

    assert wanted <= by_target.keys(), (
        f"expected a 'ready' log-plane event from both {ROLES} on fauna.admin.logs "
        f"within {LOGS_WAIT_S}s; got targets matching the wanted set: "
        f"{sorted(by_target.keys())}\n\n── bridge diagnostics ──\n{bridge_diag(nest['name'])}"
    )
    for role in ROLES:
        entry = by_target[f"{role}:ready"]
        assert f"role {role}" in entry["message"], (
            f"{role}:ready message should name its own role, got {entry['message']!r}"
        )


@pytest.mark.feature("admin-logs")
def test_atproto_bridge_ready_event_reaches_admin_logs(atproto_serving_nest):
    """The atproto PDS bridge's `ready` crosses the plane and lands on
    `fauna.admin.logs` as `atproto:ready` — the source stamped from the bridge's
    authenticated `atproto.pds` class — naming the atproto bridge, not the mail
    one (the shared `Ready` is role-aware)."""
    nest = atproto_serving_nest
    by_target = _await_targets(nest, {"atproto:ready"})
    assert "atproto:ready" in by_target, (
        f"expected an 'atproto:ready' log-plane event on fauna.admin.logs within "
        f"{LOGS_WAIT_S}s\n\n── atproto bridge log tail ──\n"
        + "\n".join(atproto_bridge_logs(nest["name"]).splitlines()[-40:])
    )
    message = by_target["atproto:ready"]["message"]
    assert message == "atproto bridge ready (role atproto.pds)", (
        f"atproto:ready should name the atproto bridge and its role, got {message!r}"
    )


# Past the router's per-source-IP cap (`fauna_conn_limit::DEFAULT_MAX_CONNS_PER_IP`,
# 256): every connection this test opens reaches the router from ONE address —
# the Docker bridge gateway the published port is NATed or proxied through,
# never the container's loopback (whose ceiling would be 1024).
_ROUTER_CONNS_TO_SHED = 320


@pytest.fixture()
def router_nest(docker_image):
    """A claimed nest with the container's :443 (the sni-router) published, so
    the test can drive the router's per-IP cap from the host."""
    (http_port,) = find_free_ports(1)
    name = f"fauna-logplane-router-{http_port}"
    start_container_with_ports(name, {443: http_port}, env={
        "FAUNA_CLAIM_CODE": CLAIM_CODE,
        "FAUNA_PORT": "3000",
    })
    try:
        wait_for_health(http_port, name)
        admin = claim_admin_api(http_port, CLAIM_CODE, handle="admin")
        yield {
            "name": name,
            "port": http_port,
            "tls_port": http_port,
            "url": f"https://127.0.0.1:{http_port}",
            "admin": admin,
        }
    finally:
        remove_container(name)


def _router_log_lines(name: str) -> str:
    """The router's own lines from the container log, for a failure message."""
    out = subprocess.run(
        ["docker", "logs", "--tail", "400", name],
        capture_output=True, text=True, timeout=30,
    )
    return "\n".join(
        line for line in (out.stdout + out.stderr).splitlines()
        if "fauna_sni_router" in line or "sni-router" in line
    )


@pytest.mark.feature("admin-logs")
def test_router_events_reach_admin_logs(router_nest):
    """The sni-router's leg of the plane: its `serving` event lands as
    `router:serving` — the source stamped from the `router` scope of its
    `/internal/router/ws` channel — and connections its per-IP cap sheds land
    as `router:per_ip_cap_shed` carrying the count."""
    import socket

    nest = router_nest
    by_target = _await_targets(nest, {"router:serving"})
    assert "router:serving" in by_target, (
        f"expected 'router:serving' on fauna.admin.logs within {LOGS_WAIT_S}s\n\n"
        f"── router log lines ──\n{_router_log_lines(nest['name'])}"
    )
    message = by_target["router:serving"]["message"]
    assert message == "router serving on port 443", message

    # Hold connections open without a ClientHello: each keeps its permit until
    # the router's peek deadline, so those past the per-IP ceiling are shed.
    # They are held until the router's OWN log shows the shed — it accepts
    # from its backlog asynchronously, so a release before then frees permits
    # it has not yet counted — and released BEFORE the admin logs are read: the
    # admin WS reaches the nest through this same router from this same IP, so
    # while they are held the cap sheds the read too.
    held = []
    try:
        for _ in range(_ROUTER_CONNS_TO_SHED):
            try:
                held.append(
                    socket.create_connection(("127.0.0.1", nest["tls_port"]), timeout=5)
                )
            except OSError:
                break
        wait_until(
            lambda: "per-IP connection cap reached" in _router_log_lines(nest["name"]),
            LOGS_WAIT_S, interval=1,
            diagnose=lambda: (
                f"the router never shed while {len(held)} connections were held\n"
                f"── router log lines ──\n{_router_log_lines(nest['name'])}"),
        )
    finally:
        for s in held:
            s.close()
    shed = _await_targets(nest, {"router:per_ip_cap_shed"})
    assert "router:per_ip_cap_shed" in shed, (
        f"expected 'router:per_ip_cap_shed' after holding {len(held)} connections\n\n"
        f"── router log lines ──\n{_router_log_lines(nest['name'])}"
    )
    message = shed["router:per_ip_cap_shed"]["message"]
    assert message.startswith("router per-IP connection cap reached; shed "), message

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
"""

from __future__ import annotations

import subprocess
import time

import pytest

from .helpers import (
    IMAGE_TAG,
    ROLES,
    admin_ws,
    bridge_diag,
    bring_bridges_to_serving,
    claim_admin_api,
    docker_build,
    find_free_ports,
    get_repo_root,
    register_primary_domain,
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
    """Build the image once for this module (or reuse via FAUNA_REUSE_IMAGE=1,
    the sanctioned path — no local nest-image builds on a dev VM). Leaves the
    shared tag in place on teardown, like the sibling mail-deploy fixtures."""
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


def _log_entries(nest) -> list[dict]:
    with admin_ws(nest) as admin:
        return admin.call("fauna.admin.logs", {})["entries"]


@pytest.mark.feature("admin-logs")
def test_bridge_ready_event_reaches_admin_logs(mail_serving_nest):
    """Both mail-bridge roles' `ready` events cross the sidecar log plane and
    land on `fauna.admin.logs` with the `<source>:<event>` target the nest
    stamps from the bridge's authenticated identity — never from the payload."""
    nest = mail_serving_nest
    deadline = time.monotonic() + LOGS_WAIT_S
    by_target: dict[str, dict] = {}
    wanted = {f"{role}:ready" for role in ROLES}
    while time.monotonic() < deadline and not wanted <= by_target.keys():
        entries = _log_entries(nest)
        by_target = {e["target"]: e for e in entries if e["target"] in wanted}
        if wanted <= by_target.keys():
            break
        time.sleep(1.0)  # sleep-ok: poll interval of a deadline loop, not a settle-wait

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
